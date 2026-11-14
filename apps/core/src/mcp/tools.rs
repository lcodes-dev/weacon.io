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
                "Where to begin in the code of this chat's folder: its crates and their modules, \
                 with how many items each holds. Call it before reading files. Plain text within \
                 `budget` bytes, {BUDGET} unless given."
            ),
            "inputSchema": {"type": "object", "properties": {"budget": budget}},
            "annotations": read_only,
        },
        {
            "name": "symbol",
            "description": "One item by its full path, such as \
                `weacon_protocol::message::Envelope`: its kind and the file and line that declare it.",
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
        McpCall::Map(args) => fit(outline(map), budget(args.budget)),
        McpCall::Symbol(args) => fit(vec![symbol(map, &args.path)], budget(args.budget)),
        McpCall::Newer => String::new(),
    }
}

/// The bytes an answer may hold when `asked` for.
fn budget(asked: Option<u32>) -> usize {
    let bytes = asked.map_or(BUDGET, |asked| asked.clamp(LEAST, MOST));
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

fn outline(map: &Index) -> Vec<String> {
    map.crates
        .iter()
        .flat_map(|krate| {
            let folder = if krate.folder.is_empty() {
                "."
            } else {
                &krate.folder
            };
            let modules = count(krate.modules.len(), "module");
            let head = format!("{} {folder}: {modules}", krate.name);
            let modules = krate.modules.iter().map(|module| {
                let name = if module.path.is_empty() {
                    "(root)"
                } else {
                    &module.path
                };
                let items = count(module.symbols.len(), "item");
                format!("  {name} {}: {items}", module.file)
            });
            std::iter::once(head).chain(modules)
        })
        .collect()
}

fn count(number: usize, what: &str) -> String {
    let plural = if number == 1 { "" } else { "s" };
    format!("{number} {what}{plural}")
}

fn symbol(map: &Index, path: &str) -> String {
    let Some(symbol) = map.symbol(path) else {
        return "No item has this path.".to_owned();
    };
    // `Index::symbol` doesn't say which module holds the item, so it is found by identity.
    let modules = map.crates.iter().flat_map(|krate| &krate.modules);
    let file = modules
        .filter(|module| module.symbols.iter().any(|item| std::ptr::eq(item, symbol)))
        .map(|module| module.file.as_str())
        .next()
        .unwrap_or("?");
    format!("{path}: {:?} at {file}:{}", symbol.kind, symbol.line)
}

/// The lines that fit in `budget` bytes, each ending with a newline; when some don't, the last
/// line says how many were left out, and is kept even when the budget is smaller than it.
fn fit(lines: Vec<String>, budget: usize) -> String {
    let total: usize = lines.iter().map(|line| line.len() + 1).sum();
    if total <= budget {
        return lines.into_iter().map(|line| line + "\n").collect();
    }
    let count = lines.len();
    let tail = |kept: usize| format!("{} more lines left out.\n", count - kept);
    let mut out = String::new();
    let mut kept = 0;
    for line in lines {
        if out.len() + line.len() + 1 + tail(kept).len() > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        kept += 1;
    }
    out + &tail(kept)
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
        assert_eq!(sizes, [Some(194), Some(120)]);
        assert_eq!(serde_json::to_string(&tools).unwrap().len(), 755);
    }

    #[test]
    fn map_outlines_each_crate_and_its_modules() {
        assert_eq!(
            answer_from(&map(None), &repo()),
            "a_b .: 2 modules\n  (root) src/lib.rs: 1 item\n  m src/m.rs: 2 items\n"
        );
    }

    #[test]
    fn symbol_says_where_an_item_is_or_that_none_has_the_path() {
        let found = answer_from(&symbol_call("a_b::m::S::go"), &repo());
        let missing = answer_from(&symbol_call("a_b::m::T"), &repo());

        assert_eq!(found, "a_b::m::S::go: Fn at src/m.rs:2\n");
        assert_eq!(missing, "No item has this path.\n");
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
        let lines = || (0..10).map(|n| format!("line {n}")).collect::<Vec<_>>();

        assert_eq!(fit(lines(), 70), lines().join("\n") + "\n");
        let cut = fit(lines(), 50);
        assert_eq!(cut, "line 0\nline 1\nline 2\n7 more lines left out.\n");
        assert!(cut.len() <= 50);
        assert_eq!(fit(lines(), 5), "10 more lines left out.\n");
        // Seventy bytes in all, so sixty-nine cut; a line that fits exactly is kept.
        assert!(fit(lines(), 69).ends_with("left out.\n"));
        let exact = fit(lines(), 51);
        assert_eq!((exact.lines().count(), exact.len()), (5, 51));
    }
}
