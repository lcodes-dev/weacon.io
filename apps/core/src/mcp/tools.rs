//! The MCP's tools: the list agents see, and the plain text each call answers.

use std::path::Path;

use rmcp::model::Tool;
use serde_json::json;
use weacon_codemap::Index;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::McpCall;

use crate::map::index_folder;

/// The answer's size when a call names none, and the bounds a named one is kept within.
const BUDGET: u32 = 6000;
const LEAST: u32 = 1000;
const MOST: u32 = 16_000;

const NO_MAP: &str = "No code map for this folder.\n";

/// The tools every run sees from its first call, as `tools/list` sends them.
pub(crate) fn listed() -> Vec<Tool> {
    let budget = json!({"type": "integer", "minimum": LEAST, "maximum": MOST});
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    let tools = json!([
        {
            "name": "map",
            "description": format!(
                "Where to begin in the code of this chat's folder: its entry points, its most \
                 central items and its crates. Call it before reading files. Plain text within \
                 `budget` bytes, {BUDGET} unless given."
            ),
            "inputSchema": {"type": "object", "properties": {"budget": budget}},
            "annotations": read_only,
        },
        {
            "name": "symbol",
            "description": "One item by its path, such as `weacon_protocol::message::Envelope`: \
                where it is, what calls, uses and tests it, and what it calls. A partial path \
                lists the items it ends.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}, "budget": budget},
                "required": ["path"],
            },
            "annotations": read_only,
        },
    ]);
    serde_json::from_value(tools).unwrap_or_default()
}

/// The answer to `call` from the code in `folder`; it reads the folder, so it blocks.
///
/// # Errors
/// When the folder can't be read.
pub(crate) fn answer(call: &McpCall, folder: &Path) -> Result<String, Fault> {
    index_folder(folder).map(|(map, _)| answer_from(call, &map))
}

/// [`answer`], with a folder that can't be read logged and answered as having no map.
pub(crate) fn answer_or_none(call: &McpCall, folder: &Path, log: &Logger) -> String {
    answer(call, folder).unwrap_or_else(|fault| {
        log.warn("mcp map failed", &[("err", Field::error(&fault))]);
        NO_MAP.to_owned()
    })
}

fn answer_from(call: &McpCall, map: &Index) -> String {
    if map.crates.is_empty() {
        return NO_MAP.to_owned();
    }
    match call {
        McpCall::Map(args) => map.start_here(budget(args.budget)),
        McpCall::Symbol(args) => map.item(&args.path, budget(args.budget)),
        McpCall::Newer => String::new(),
    }
}

/// The bytes an answer may hold when `asked` for.
fn budget(asked: Option<u32>) -> usize {
    let bytes = asked.map_or(BUDGET, |asked| asked.clamp(LEAST, MOST));
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use weacon_codemap::{SourceFile, index};
    use weacon_protocol::{MapArgs, SymbolArgs};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    fn repo() -> Index {
        index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", "mod m;\npub fn run() {}\n"),
            file("src/m.rs", "pub struct S;\nimpl S { pub fn go() {} }\n"),
        ])
        .unwrap()
    }

    fn map(budget: Option<u32>) -> McpCall {
        McpCall::Map(MapArgs { budget })
    }

    fn symbol_call(path: &str) -> McpCall {
        McpCall::Symbol(SymbolArgs {
            path: path.to_owned(),
            budget: None,
        })
    }

    #[test]
    fn the_tool_list_and_each_description_keep_their_size() {
        let tools = listed();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        let sizes: Vec<_> = tools
            .iter()
            .map(|tool| tool.description.as_deref().map(str::len))
            .collect();

        assert_eq!(names, ["map", "symbol"]);
        assert_eq!(sizes, [Some(189), Some(170)]);
        assert_eq!(serde_json::to_string(&tools).unwrap().len(), 800);
    }

    #[test]
    fn map_says_where_to_begin() {
        assert_eq!(
            answer_from(&map(None), &repo()),
            "Start here: 1 crates, 3 items.\nEntry points:\nMost central items:\n\
             - struct a_b::m::S  src/m.rs:1\n- fn a_b::m::S::go  src/m.rs:2\n\
             - fn a_b::run  src/lib.rs:2\nCrates:\n- a_b  .  3 items\n"
        );
    }

    #[test]
    fn symbol_says_where_an_item_is_or_which_items_end_with_the_path() {
        let found = answer_from(&symbol_call("a_b::m::S::go"), &repo());
        let close = answer_from(&symbol_call("S::go"), &repo());

        assert_eq!(found, "fn a_b::m::S::go  src/m.rs:2\n");
        assert_eq!(
            close,
            "No item S::go.\nItems with that name:\n- fn a_b::m::S::go  src/m.rs:2\n"
        );
    }

    #[test]
    fn a_folder_without_crates_has_no_map_and_one_that_cant_be_read_says_so_once() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        let (log, records) = recording_logger();

        assert_eq!(answer_or_none(&map(None), &dir, &log), NO_MAP);
        assert_eq!(answer_or_none(&map(None), &dir.join("gone"), &log), NO_MAP);
        let gone = answer(&map(None), &dir.join("gone")).unwrap_err();
        assert_eq!(gone.code().as_str(), "map.read_failed");
        assert_eq!(weacon_testing::messages(&records), ["mcp map failed"]);
    }

    #[test]
    fn a_budget_is_kept_within_its_bounds() {
        assert_eq!(budget(None), 6000);
        assert_eq!(budget(Some(10)), 1000);
        assert_eq!(budget(Some(1500)), 1500);
        assert_eq!(budget(Some(u32::MAX)), 16_000);
    }

    #[test]
    fn an_answer_ends_with_a_whole_line_within_its_budget() {
        let items = (0..400).fold(String::new(), |items, n| {
            items + &format!("pub fn item_{n}() {{}}\n")
        });
        let large = index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", &items),
        ])
        .unwrap();

        for call in [map(Some(1000)), symbol_call("item_1")] {
            let answer = answer_from(&call, &large);
            assert!(answer.len() <= 1000 && answer.ends_with('\n'), "{answer}");
        }
    }
}
