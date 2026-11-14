//! The MCP's tools: the list agents see, and the plain text each call answers.

use std::path::Path;

use rmcp::model::Tool;
use weacon_codemap::{Index, SourceFile, index};
use weacon_protocol::McpCall;

use crate::map::source_files;

/// The answer's size when a call names none, and the bounds a named one is kept within.
pub(crate) const BUDGET: usize = 6000;
const LEAST: usize = 1000;
const MOST: usize = 16_000;

/// The tools every run sees from its first call, as `tools/list` sends them.
const LISTED: &str = r#"[
{"name":"map","description":"Where to begin in the code of this chat's folder: its crates and their modules, with how many items each holds. Call it before reading files. Plain text within `budget` bytes, 6000 unless given.","inputSchema":{"type":"object","properties":{"budget":{"type":"integer","minimum":1000,"maximum":16000}}},"annotations":{"readOnlyHint":true,"openWorldHint":false}},
{"name":"symbol","description":"One item by its full path, such as `weacon_protocol::frame::Envelope`: its kind and the file and line that declare it.","inputSchema":{"type":"object","properties":{"path":{"type":"string","maxLength":1000},"budget":{"type":"integer","minimum":1000,"maximum":16000}},"required":["path"]},"annotations":{"readOnlyHint":true,"openWorldHint":false}}
]"#;

pub(crate) const NO_MAP: &str = "No code map for this folder.\n";
const LONGEST_PATH: usize = 1000;

pub(crate) fn listed() -> Vec<Tool> {
    serde_json::from_str(LISTED).unwrap_or_default()
}

/// The bytes an answer may hold when `asked` for.
pub(crate) fn budget(asked: Option<u32>) -> usize {
    asked.map_or(BUDGET, |asked| {
        usize::try_from(asked).unwrap_or(MOST).clamp(LEAST, MOST)
    })
}

/// The answer to `call` from the code in `folder`; it reads the folder, so it blocks.
pub(crate) fn answer(call: &McpCall, folder: &Path) -> String {
    let mut files = Vec::new();
    if source_files(folder, "", &mut files).is_err() {
        return NO_MAP.to_owned();
    }
    answer_from(call, &files)
}

fn answer_from(call: &McpCall, files: &[SourceFile]) -> String {
    let Some(map) = index(files).ok().filter(|map| !map.crates.is_empty()) else {
        return NO_MAP.to_owned();
    };
    match call {
        McpCall::Map(args) => fit(outline(&map), budget(args.budget)),
        McpCall::Symbol(args) => fit(symbol(&map, &args.path), budget(args.budget)),
        McpCall::Newer => "No such tool.\n".to_owned(),
    }
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
            let head = format!(
                "{} {folder}: {}",
                krate.name,
                count(krate.modules.len(), "module")
            );
            let modules = krate.modules.iter().map(|module| {
                let name = if module.path.is_empty() {
                    "(root)"
                } else {
                    &module.path
                };
                format!(
                    "  {name} {}: {}",
                    module.file,
                    count(module.symbols.len(), "item")
                )
            });
            std::iter::once(head).chain(modules)
        })
        .collect()
}

fn count(number: usize, what: &str) -> String {
    let plural = if number == 1 { "" } else { "s" };
    format!("{number} {what}{plural}")
}

fn symbol(map: &Index, path: &str) -> Vec<String> {
    let found = (path.len() <= LONGEST_PATH)
        .then(|| map.symbol(path))
        .flatten()
        .and_then(|symbol| {
            let module = map
                .crates
                .iter()
                .flat_map(|krate| &krate.modules)
                .find(|module| module.symbols.iter().any(|item| std::ptr::eq(item, symbol)))?;
            Some(format!(
                "{path}: {:?} at {}:{}",
                symbol.kind, module.file, symbol.line
            ))
        });
    vec![found.unwrap_or_else(|| "No item has this path.".to_owned())]
}

/// The lines that fit in `budget` bytes, each ending with a newline; when some don't, the last
/// line says how many were left out, and is kept even when the budget is smaller than it.
pub(crate) fn fit(lines: Vec<String>, budget: usize) -> String {
    let total: usize = lines.iter().map(|line| line.len() + 1).sum();
    if total <= budget {
        return lines.into_iter().map(|line| line + "\n").collect();
    }
    let count = lines.len();
    let mut out = String::new();
    let mut kept = 0;
    for line in lines {
        let tail = format!("{} more lines left out.\n", count - kept);
        if out.len() + line.len() + 1 + tail.len() > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        kept += 1;
    }
    out + &format!("{} more lines left out.\n", count - kept)
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{MapArgs, SymbolArgs};

    use super::*;

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    fn repo() -> Vec<SourceFile> {
        vec![
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", "mod m;\npub fn run() {}\n"),
            file("src/m.rs", "pub struct S;\nimpl S { pub fn go() {} }\n"),
        ]
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
        assert_eq!(sizes, [Some(194), Some(118)]);
        assert_eq!(serde_json::to_string(&tools).unwrap().len(), 770);
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
        let long = answer_from(&symbol_call(&"a_b::".repeat(300)), &repo());

        assert_eq!(found, "a_b::m::S::go: Fn at src/m.rs:2\n");
        assert_eq!(missing, "No item has this path.\n");
        assert_eq!(long, missing);
        assert_eq!(answer_from(&McpCall::Newer, &repo()), "No such tool.\n");
    }

    #[test]
    fn a_folder_without_crates_has_no_map() {
        let dir = weacon_testing::TempDir::new().unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        assert_eq!(answer(&map(None), &dir), NO_MAP);
        assert_eq!(answer(&map(None), &dir.join("gone")), NO_MAP);
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
    }
}
