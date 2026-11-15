use std::collections::{BTreeMap, BTreeSet};

use crate::facts::{Facts, FileKind};
use crate::{Anchor, Edge, EdgeKind};

/// How many calls apart two steps of a flow may be.
const REACH: usize = 4;

/// A flow pinned in a document, such as `flow:a::run->a::serve` in `docs/serving.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Flow {
    pub(crate) steps: Vec<String>,
    pub(crate) file: String,
    /// Counting from 1.
    pub(crate) line: usize,
    /// The first place the code no longer follows it, if any.
    pub(crate) breaks: Option<Break>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Break {
    /// No item has this path.
    Missing(String),
    /// The first step doesn't reach the second through calls.
    Unreached(String, String),
}

/// The flows a document pins, with their lines: pins sit in text, code spans or links.
pub(crate) fn written(text: &str) -> Vec<(Vec<String>, usize)> {
    let mut found = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        for (start, _) in line.match_indices("flow:") {
            let before = line
                .get(..start)
                .and_then(|before| before.chars().next_back());
            if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                continue;
            }
            let rest = line.get(start..).unwrap_or_default();
            let end = rest
                .find(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '`' | ')' | '"' | '\'' | ',' | '#')
                })
                .unwrap_or(rest.len());
            let pin = rest
                .get(..end)
                .unwrap_or_default()
                .trim_end_matches(['.', '*', ']', '>', ';', ':']);
            if let Ok(Anchor::Flow(steps)) = pin.parse() {
                found.push((steps, number));
            }
        }
    }
    found
}

/// Checks each flow the documents pin against the items and the calls between them.
pub(crate) fn check<T>(facts: &[Facts], items: &BTreeMap<String, T>, edges: &[Edge]) -> Vec<Flow> {
    let written = facts.iter().flat_map(|facts| {
        let flows = match &facts.kind {
            FileKind::Doc { flows, .. } => flows.as_slice(),
            FileKind::Manifest(_) | FileKind::Rust { .. } => &[],
        };
        flows
            .iter()
            .map(|(steps, line)| (facts.path.clone(), steps.clone(), *line))
    });
    let calls = Calls::new(edges);
    let mut flows: Vec<Flow> = written
        .into_iter()
        .map(|(file, steps, line)| {
            let missing = steps
                .iter()
                .find(|step| !items.contains_key(*step))
                .cloned()
                .map(Break::Missing);
            let unreached = || {
                steps.windows(2).find_map(|pair| match pair {
                    [from, to] if !calls.reach(from, to) => {
                        Some(Break::Unreached(from.clone(), to.clone()))
                    }
                    _ => None,
                })
            };
            let breaks = missing.or_else(unreached);
            Flow {
                steps,
                file,
                line,
                breaks,
            }
        })
        .collect();
    flows.sort_by(|one, other| (&one.file, one.line).cmp(&(&other.file, other.line)));
    flows
}

/// Each item's calls, by the caller's path.
struct Calls<'a>(BTreeMap<&'a str, Vec<&'a str>>);

impl<'a> Calls<'a> {
    fn new(edges: &'a [Edge]) -> Self {
        let mut calls: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for edge in edges.iter().filter(|edge| edge.kind == EdgeKind::Calls) {
            calls.entry(&edge.from).or_default().push(&edge.to);
        }
        Self(calls)
    }

    /// Whether `from` calls `to`, or calls what calls it, at most `REACH` calls deep.
    fn reach(&self, from: &str, to: &str) -> bool {
        let mut seen = BTreeSet::from([from]);
        let mut frontier = vec![from];
        for _ in 0..REACH {
            let next: Vec<&str> = frontier
                .iter()
                .flat_map(|step| self.0.get(step).into_iter().flatten().copied())
                .filter(|step| seen.insert(step))
                .collect();
            if next.contains(&to) {
                return true;
            }
            frontier = next;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::test_support::files;

    #[test]
    fn finds_pins_in_text_code_and_links() {
        let text = "Runs `flow:a::run->a::m::go`, then [here](flow:a::go->a::stop#00ff).\n\
                    flow:a::x->a::y. flow:a::only flow:not a flow\n\
                    **flow:a::b->a::c** <flow:a::d->a::e>; workflow:a::f->a::g\n";
        let steps = |list: &[&str]| {
            list.iter()
                .map(|step| (*step).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            written(text),
            [
                (steps(&["a::run", "a::m::go"]), 1),
                (steps(&["a::go", "a::stop"]), 1),
                (steps(&["a::x", "a::y"]), 2),
                (steps(&["a::b", "a::c"]), 3),
                (steps(&["a::d", "a::e"]), 3),
            ]
        );
    }

    #[test]
    fn checks_each_step_against_calls() {
        let index = index(&files(&[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "fn one() { two(); }\nfn two() { three(); }\nfn three() { four(); }\nfn four() { five(); }\n\
                 fn five() { six(); }\nfn six() {}\nfn alone() {}\n",
            ),
            (
                "docs/flows.md",
                "flow:a::one->a::five\nflow:a::one->a::six\nflow:a::two->a::alone\nflow:a::one->a::gone\n",
            ),
            ("README.md", "flow:a::six->a::one\n"),
        ]))
        .unwrap();
        assert_eq!(
            index.flows(10_000),
            "Flows pinned in the docs: 5.\n- README.md:1  a::six -> a::one: breaks: a::six doesn't reach a::one through calls\n\
             - docs/flows.md:1  a::one -> a::five: holds\n\
             - docs/flows.md:2  a::one -> a::six: breaks: a::one doesn't reach a::six through calls\n\
             - docs/flows.md:3  a::two -> a::alone: breaks: a::two doesn't reach a::alone through calls\n\
             - docs/flows.md:4  a::one -> a::gone: breaks: no item a::gone\n"
        );
    }
}
