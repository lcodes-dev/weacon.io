//! The MCP's tools: the list agents see, and the plain text each call answers.

use std::borrow::Cow;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};
use weacon_codemap::{Churn, Index};
use weacon_protocol::McpCall;

/// The answer's size when a call names none, and the bounds a named one is kept within.
const BUDGET: u32 = 6000;
const LEAST: u32 = 1000;
const MOST: u32 = 16_000;

/// How many of the latest commits `hotspots` counts changes in.
pub(crate) const CHURN_COMMITS: u32 = 500;

pub(crate) const NO_MAP: &str = "No code map for this folder.\n";

/// Longer than any item's path, and too long to echo back within a budget.
const LONGEST_PATH: usize = 500;
const NO_ITEM: &str = "No item has this path.\n";

/// The tools Claude may call without asking.
pub(crate) const UP_FRONT: [&str; 2] = [McpCall::MAP, McpCall::SYMBOL];

/// The tools every run sees from its first call, as `tools/list` sends them.
pub(crate) fn listed() -> Vec<Tool> {
    let budget = json!({"type": "integer", "minimum": LEAST, "maximum": MOST});
    vec![
        tool(
            McpCall::MAP,
            format!(
                "Where to begin in the code of this chat's folder: its entry points, its most \
                 central items and its crates. Call it before reading files. Plain text within \
                 `budget` bytes, {BUDGET} unless given."
            ),
            json!({"budget": budget}),
            &[],
        ),
        tool(
            McpCall::SYMBOL,
            "One item by its path, such as `weacon_protocol::message::Envelope`: where it is, \
             what calls, uses and tests it, and what it calls. A partial path lists the items it \
             ends.",
            json!({"path": {"type": "string"}, "budget": budget}),
            &["path"],
        ),
        tool(
            McpCall::DOCS_FOR,
            "The doc lines that pin one item by its path, as an item or a step of a flow; read \
             them before changing it.",
            json!({"path": {"type": "string"}, "budget": budget}),
            &["path"],
        ),
        tool(
            McpCall::FLOW,
            "The flows the docs pin and where the code breaks them; call it before changing \
             code a flow passes through.",
            json!({"budget": budget}),
            &[],
        ),
        tool(
            McpCall::HOTSPOTS,
            "The files that changed most in recent commits, weighed by how much depends on \
             them; changes there need the most care.",
            json!({"budget": budget}),
            &[],
        ),
        propose(),
    ]
}

/// The one tool that writes, and only what a person then accepts.
fn propose() -> Tool {
    writes(tool(
        McpCall::PROPOSE,
        "Proposes a change to a doc or task of this chat's workspace; nothing changes until \
         a person accepts it. Only when the person asks. `change` is one of {kind: \
         new_task, title}, {kind: new_doc, content: {title, body}}, {kind: task_edit, task, \
         base, edits: [{kind: title, title} or {kind: status, status}]}, {kind: doc_edit, \
         doc, base, content}.",
        json!({"change": {"type": "object"}}),
        &["change"],
    ))
}

/// `tool`, as one that writes: what it changes, a person accepts first.
fn writes(tool: Tool) -> Tool {
    let hints = ToolAnnotations::new()
        .read_only(false)
        .destructive(false)
        .open_world(false);
    tool.annotate(hints)
}

/// A read-only tool taking an object with `properties`, of which `required` must be given.
fn tool(
    name: &'static str,
    description: impl Into<Cow<'static, str>>,
    properties: Value,
    required: &[&str],
) -> Tool {
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), "object".into());
    schema.insert("properties".to_owned(), properties);
    if !required.is_empty() {
        schema.insert("required".to_owned(), json!(required));
    }
    let hints = ToolAnnotations::new().read_only(true).open_world(false);
    Tool::new(name, description, schema).annotate(hints)
}

// Docs can pin flows in a folder with no crate, so `flow` answers even without a code map.
pub(crate) fn answer_from(call: &McpCall, map: &Index, churn: &[Churn]) -> String {
    match call {
        McpCall::Flow(args) => map.flows(budget(args.budget)),
        _ if map.crates.is_empty() => NO_MAP.to_owned(),
        McpCall::Map(args) => map.start_here(budget(args.budget)),
        McpCall::Hotspots(args) => map.hotspots(churn, budget(args.budget)),
        McpCall::Symbol(args) | McpCall::DocsFor(args) if args.path.len() > LONGEST_PATH => {
            NO_ITEM.to_owned()
        }
        McpCall::Symbol(args) => map.item(&args.path, budget(args.budget)),
        McpCall::DocsFor(args) => map.docs_for(&args.path, budget(args.budget)),
        McpCall::Propose(_) | McpCall::Newer => String::new(),
    }
}

/// The bytes an answer may hold when `asked` for.
pub(crate) fn budget(asked: Option<u32>) -> usize {
    let bytes = asked.map_or(BUDGET, |asked| asked.clamp(LEAST, MOST));
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use weacon_codemap::{SourceFile, index};
    use weacon_protocol::{FlowArgs, MapArgs, SymbolArgs};

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

        assert_eq!(
            names,
            ["map", "symbol", "docs_for", "flow", "hotspots", "propose"]
        );
        assert_eq!(names.get(..2), Some(UP_FRONT.as_slice()));
        assert_eq!(
            sizes,
            [
                Some(189),
                Some(170),
                Some(106),
                Some(106),
                Some(117),
                Some(341)
            ]
        );
        assert_eq!(serde_json::to_string(&tools).unwrap().len(), 2329);
    }

    #[test]
    fn map_says_where_to_begin() {
        assert_eq!(
            answer_from(&map(None), &repo(), &[]),
            "Start here: 1 crates, 3 items.\nEntry points:\nMost central items:\n\
             - struct a_b::m::S  src/m.rs:1\n- fn a_b::m::S::go  src/m.rs:2\n\
             - fn a_b::run  src/lib.rs:2\nCrates:\n- a_b  .  3 items\n"
        );
    }

    #[test]
    fn symbol_says_where_an_item_is_or_which_items_end_with_the_path() {
        let found = answer_from(&symbol_call("a_b::m::S::go"), &repo(), &[]);
        let close = answer_from(&symbol_call("S::go"), &repo(), &[]);

        assert_eq!(found, "fn a_b::m::S::go  src/m.rs:2\n");
        assert_eq!(
            close,
            "No item S::go.\nItems with that name:\n- fn a_b::m::S::go  src/m.rs:2\n"
        );
    }

    #[test]
    fn docs_for_lists_the_lines_that_pin_an_item() {
        let docs = index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", "pub fn run() {}\n"),
            file("docs/a.md", "Begin at sym:a_b::run.\n"),
        ])
        .unwrap();
        let docs_for = |path: &str| {
            let args = SymbolArgs {
                path: path.to_owned(),
                budget: None,
            };
            answer_from(&McpCall::DocsFor(args), &docs, &[])
        };

        assert_eq!(
            docs_for("a_b::run"),
            "Docs that pin a_b::run: 1.\n- docs/a.md:1  Begin at sym:a_b::run.\n"
        );
        assert_eq!(docs_for(&"x".repeat(LONGEST_PATH + 1)), NO_ITEM);
    }

    #[test]
    fn flow_says_where_each_pinned_flow_breaks() {
        let docs = index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", "pub fn run() {}\npub fn go() {}\n"),
            file("docs/a.md", "See flow:a_b::run->a_b::gone.\n"),
        ])
        .unwrap();

        let flows = answer_from(&McpCall::Flow(FlowArgs::default()), &docs, &[]);

        assert_eq!(
            flows,
            "Flows pinned in the docs: 1.\n- docs/a.md:1  a_b::run -> a_b::gone: breaks: no item a_b::gone\n"
        );
    }

    #[test]
    fn hotspots_answer_from_the_churn_given() {
        let churn =
            [("src/m.rs", 2), ("src/lib.rs", 5), ("Cargo.toml", 9)].map(|(file, commits)| Churn {
                file: file.to_owned(),
                commits,
            });
        let hotspots = McpCall::Hotspots(MapArgs::default());
        let empty = index(&[file("docs/a.md", "# A\n")]).unwrap();

        assert_eq!(
            answer_from(&hotspots, &repo(), &churn),
            "Hotspots: files that change often and that much depends on.\n\
             - src/lib.rs  5 commits, 0 edges in\n- src/m.rs  2 commits, 0 edges in\n"
        );
        assert_eq!(answer_from(&hotspots, &empty, &churn), NO_MAP);
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

        let start = answer_from(&map(Some(1000)), &large, &[]);
        assert!(start.len() <= 1000 && start.ends_with('\n'), "{start}");
        // Too many lines for the budget, so each ends by counting those it left out.
        for call in [symbol, flow] {
            let answer = answer_from(&call, &large, &[]);
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
            answer_from(&at_most, &repo(), &[]),
            format!("No item {}.\n", "x".repeat(500))
        );
        assert_eq!(answer_from(&longer, &repo(), &[]), NO_ITEM);
    }
}
