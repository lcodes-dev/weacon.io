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

/// Longer than any item's path, and too long to echo back within a budget.
const LONGEST_PATH: usize = 500;
const NO_ITEM: &str = "No item has this path.\n";

/// The tools Claude may call without asking.
pub(crate) const UP_FRONT: [&str; 2] = ["map", "symbol"];

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
        {
            "name": "flow",
            "description": "The flows the docs pin and where the code breaks them; call it \
                before changing code a flow passes through.",
            "inputSchema": {"type": "object", "properties": {"budget": budget}},
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

/// What `answer` gives the index of `folder` within `budget`, or that it has no map; it blocks.
///
/// # Errors
/// When the folder can't be read.
pub(crate) fn answered(
    folder: &Path,
    budget: Option<u32>,
    answer: impl FnOnce(&Index, usize) -> String,
) -> Result<String, Fault> {
    let (map, _) = index_folder(folder)?;
    Ok(if map.crates.is_empty() {
        NO_MAP.to_owned()
    } else {
        answer(&map, self::budget(budget))
    })
}

/// [`answer`], with a folder that can't be read logged and answered as having no map.
pub(crate) fn answer_or_none(call: &McpCall, folder: &Path, log: &Logger) -> String {
    answer(call, folder).unwrap_or_else(|fault| {
        log.warn("mcp map failed", &[("err", Field::error(&fault))]);
        NO_MAP.to_owned()
    })
}

// Docs can pin flows in a folder with no crate, so `flow` answers even without a code map.
fn answer_from(call: &McpCall, map: &Index) -> String {
    match call {
        McpCall::Flow(args) => map.flows(budget(args.budget)),
        _ if map.crates.is_empty() => NO_MAP.to_owned(),
        McpCall::Map(args) => map.start_here(budget(args.budget)),
        McpCall::Symbol(args) if args.path.len() > LONGEST_PATH => NO_ITEM.to_owned(),
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
    use weacon_protocol::{FlowArgs, MapArgs, SymbolArgs};
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

        assert_eq!(names, ["map", "symbol", "flow"]);
        assert_eq!(names.get(..2), Some(UP_FRONT.as_slice()));
        assert_eq!(sizes, [Some(189), Some(170), Some(106)]);
        assert_eq!(serde_json::to_string(&tools).unwrap().len(), 1103);
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
    fn flow_says_where_each_pinned_flow_breaks() {
        let docs = index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", "pub fn run() {}\npub fn go() {}\n"),
            file("docs/a.md", "See flow:a_b::run->a_b::gone.\n"),
        ])
        .unwrap();

        let flows = answer_from(&McpCall::Flow(FlowArgs::default()), &docs);

        assert_eq!(
            flows,
            "Flows pinned in the docs: 1.\n- docs/a.md:1  a_b::run -> a_b::gone: breaks: no item a_b::gone\n"
        );
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
        let calls = (0..400).fold("pub fn hub() {}\n".to_owned(), |text, n| {
            text + &format!("pub fn item_{n}() {{ hub(); }}\n")
        });
        let pins = (0..400).fold(String::new(), |text, n| {
            text + &format!("flow:a_b::item_{n}->a_b::item_{}\n", n + 1)
        });
        let large = index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", &calls),
            file("docs/flows.md", &pins),
        ])
        .unwrap();
        let symbol = McpCall::Symbol(SymbolArgs {
            path: "a_b::hub".to_owned(),
            budget: Some(1000),
        });
        let flow = McpCall::Flow(FlowArgs { budget: Some(1000) });

        let start = answer_from(&map(Some(1000)), &large);
        assert!(start.len() <= 1000 && start.ends_with('\n'), "{start}");
        // Too many lines for the budget, so each ends by counting those it left out.
        for call in [symbol, flow] {
            let answer = answer_from(&call, &large);
            assert!(answer.len() <= 1000, "{answer}");
            let last = answer.lines().last().unwrap();
            assert!(last.starts_with("… ") && answer.ends_with('\n'), "{answer}");
        }
    }

    #[test]
    fn a_path_too_long_to_echo_has_no_item() {
        let at_most = symbol_call(&"x".repeat(LONGEST_PATH));
        let longer = symbol_call(&"x".repeat(LONGEST_PATH + 1));

        assert_eq!(
            answer_from(&at_most, &repo()),
            format!("No item {}.\n", "x".repeat(500))
        );
        assert_eq!(answer_from(&longer, &repo()), NO_ITEM);
    }
}
