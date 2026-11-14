//! Answers about an index as plain text within a byte budget, most central first.

mod item;

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::graph::nodes;
use crate::{Break, ContractKind, EdgeKind, Index};

/// How often a file changed, as git counts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Churn {
    pub file: String,
    pub commits: u32,
}

/// Room kept for the last line, which says how many lines were left out.
const TAIL: usize = "… 4294967295 more lines\n".len();

/// How many of a list's entries are named before the rest are counted.
const NAMED: usize = 5;

/// Lines that stop before the budget, then say how many were left out; that last line is kept
/// even when the budget is smaller than it.
struct Text {
    lines: Vec<String>,
    budget: usize,
}

impl Text {
    const fn new(budget: usize) -> Self {
        Self {
            lines: Vec::new(),
            budget,
        }
    }

    fn line(&mut self, line: &str) {
        self.lines.push(format!("{line}\n"));
    }

    /// Every line when they all fit; else the first ones that fit beside the last line.
    fn finish(self) -> String {
        let all: usize = self.lines.iter().map(String::len).sum();
        if all <= self.budget {
            return self.lines.concat();
        }
        let mut out = String::new();
        let mut kept = 0;
        for line in &self.lines {
            if out.len() + line.len() + TAIL > self.budget {
                break;
            }
            out.push_str(line);
            kept += 1;
        }
        let left_out = self.lines.len() - kept;
        let lines = if left_out == 1 { "line" } else { "lines" };
        out + &format!("… {left_out} more {lines}\n")
    }
}

impl Index {
    /// Where to begin reading: entry points, the most central items, and the crates.
    #[must_use]
    pub fn start_here(&self, budget: usize) -> String {
        let mut text = Text::new(budget);
        text.line(&format!(
            "Start here: {} crates, {} items.",
            self.crates.len(),
            self.items.len()
        ));
        self.entry_points(&mut text);
        text.line("Most central items:");
        for path in self.central.iter().take(15) {
            if let Some(found) = self.items.get(path) {
                text.line(&format!("- {}", found.describe(path)));
            }
        }
        text.line("Crates:");
        for krate in &self.crates {
            let items: usize = krate
                .modules
                .iter()
                .map(|module| module.symbols.len())
                .sum();
            text.line(&format!(
                "- {}  {}  {items} items",
                krate.name,
                folder(&krate.folder)
            ));
        }
        text.finish()
    }

    /// How the code is split: which crates use which, and the areas of files that go together.
    #[must_use]
    pub fn architecture(&self, budget: usize) -> String {
        let mut text = Text::new(budget);
        text.line(&format!(
            "Architecture: {} crates in {} areas.",
            self.crates.len(),
            self.areas.len()
        ));
        text.line("Crates and the crates they use, with how many edges:");
        let mut uses: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.kind != EdgeKind::Tests)
        {
            let (from, to) = (crate_of(&edge.from), crate_of(&edge.to));
            if from != to {
                *uses.entry(from).or_default().entry(to).or_default() += 1;
            }
        }
        for (from, targets) in &uses {
            let mut targets: Vec<(&&str, &usize)> = targets.iter().collect();
            targets.sort_by_key(|(to, count)| (Reverse(**count), **to));
            let listed: Vec<String> = targets
                .iter()
                .map(|(to, count)| format!("{to} {count}"))
                .collect();
            text.line(&format!("- {from} uses {}", listed.join(", ")));
        }
        text.line("Areas, each named by its most central file, largest first:");
        let mut areas: Vec<_> = self.areas.iter().collect();
        areas.sort_by_key(|area| (Reverse(area.files.len()), &area.id));
        for area in areas {
            text.line(&format!(
                "- area:{} ({} files): {}",
                area.id,
                area.files.len(),
                named(&area.files)
            ));
        }
        text.finish()
    }

    /// Files that change often and that much depends on: commits times one more than the edges
    /// into the file's items.
    #[must_use]
    pub fn hotspots(&self, churn: &[Churn], budget: usize) -> String {
        let mut edges_in: BTreeMap<&str, u64> = BTreeMap::new();
        let modules: BTreeSet<&str> = nodes(&self.crates)
            .into_iter()
            .map(|node| node.module.file.as_str())
            .collect();
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.kind != EdgeKind::Tests)
        {
            if let Some(found) = self.items.get(&edge.to) {
                *edges_in.entry(&found.file).or_default() += 1;
            }
        }
        let mut scored: Vec<(u64, &Churn, u64)> = churn
            .iter()
            .filter(|changed| modules.contains(changed.file.as_str()))
            .map(|changed| {
                let depended = edges_in.get(changed.file.as_str()).copied().unwrap_or(0);
                (
                    u64::from(changed.commits) * (depended + 1),
                    changed,
                    depended,
                )
            })
            .collect();
        scored.sort_by_key(|(score, changed, _)| (Reverse(*score), &changed.file));
        let mut text = Text::new(budget);
        text.line("Hotspots: files that change often and that much depends on.");
        for (_, changed, depended) in scored {
            let commits = changed.commits;
            text.line(&format!(
                "- {}  {commits} commits, {depended} edges in",
                changed.file
            ));
        }
        text.finish()
    }

    /// The flows the documents pin, and where each breaks.
    #[must_use]
    pub fn flows(&self, budget: usize) -> String {
        let mut text = Text::new(budget);
        text.line(&format!("Flows pinned in the docs: {}.", self.flows.len()));
        for flow in &self.flows {
            let state = match &flow.breaks {
                None => "holds".to_owned(),
                Some(Break::Missing(step)) => format!("breaks: no item {step}"),
                Some(Break::Unreached(from, to)) => {
                    format!("breaks: {from} doesn't reach {to} through calls")
                }
            };
            text.line(&format!(
                "- {}:{}  {}: {state}",
                flow.file,
                flow.line,
                flow.steps.join(" -> ")
            ));
        }
        text.finish()
    }

    /// The contracts of one kind, or of every kind, in order.
    #[must_use]
    pub fn contracts(&self, kind: Option<ContractKind>, budget: usize) -> String {
        let mut text = Text::new(budget);
        let chosen = self
            .contracts
            .iter()
            .filter(|contract| kind.is_none_or(|kind| contract.kind == kind));
        for contract in chosen {
            let line = format!(
                "- {}  {}  {}:{}",
                contract.describe(),
                contract.item,
                contract.file,
                contract.line
            );
            text.line(&line);
        }
        text.finish()
    }

    /// The binaries' `main`s, the subcommands and the routes.
    fn entry_points(&self, text: &mut Text) {
        text.line("Entry points:");
        for node in nodes(&self.crates) {
            let file = &node.module.file;
            let binary = file.ends_with("src/main.rs")
                || file.starts_with("src/bin/")
                || file.contains("/src/bin/");
            let main = node
                .module
                .symbols
                .iter()
                .find(|symbol| symbol.name == "main");
            if let Some(main) = main.filter(|_| binary) {
                let krate = &node.krate.name;
                text.line(&format!("- main of {krate}  {file}:{}", main.line));
            }
        }
        let entries = self.contracts.iter().filter(|contract| {
            matches!(contract.kind, ContractKind::Command | ContractKind::Route)
        });
        for contract in entries {
            text.line(&format!(
                "- {}  {}:{}",
                contract.describe(),
                contract.file,
                contract.line
            ));
        }
    }
}

/// The first few of `names`, and how many more there are.
fn named(names: &[String]) -> String {
    let shown: Vec<&str> = names.iter().take(NAMED).map(String::as_str).collect();
    match names.len().saturating_sub(NAMED) {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}

fn crate_of(path: &str) -> &str {
    path.split("::").next().unwrap_or_default()
}

fn folder(folder: &str) -> &str {
    if folder.is_empty() { "." } else { folder }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::test_support::files;

    /// A binary that runs a library, which declares a route and a code.
    fn repo() -> Index {
        index(&files(&[
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
            ("app/src/main.rs", "fn main() { lib::run(); }\n"),
            ("lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
            (
                "lib/src/lib.rs",
                "mod store;\npub trait Show {}\npub struct Page;\nimpl Show for Page {}\n\
                 pub fn run() { store::keep(); store::keep_more(); }\n\
                 fn router() { Router::new().route(\"/page\", get(run)); }\n\
                 #[cfg(test)]\nmod tests { #[test]\n fn runs() { super::run(); } }\n",
            ),
            (
                "lib/src/store.rs",
                "pub fn keep() { code!(\"store.full\"); }\npub fn keep_more() { keep(); }\n",
            ),
        ]))
        .unwrap()
    }

    #[test]
    fn lists_every_binary() {
        let index = index(&files(&[
            ("Cargo.toml", "[package]\nname = \"root\"\n"),
            ("src/lib.rs", "pub fn main() {}\n"),
            ("src/bin/tool.rs", "fn main() {}\n"),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
            ("app/src/bin/other.rs", "fn main() {}\n"),
        ]))
        .unwrap();
        let found = index.start_here(10_000);
        let mains: Vec<&str> = found
            .lines()
            .filter(|line| line.starts_with("- main of"))
            .collect();
        assert_eq!(
            mains,
            [
                "- main of root  src/bin/tool.rs:1",
                "- main of app  app/src/bin/other.rs:1"
            ]
        );
    }

    #[test]
    fn starts_with_entry_points_then_central_items() {
        assert_eq!(
            repo().start_here(10_000),
            "Start here: 2 crates, 7 items.\nEntry points:\n\
             - main of app  app/src/main.rs:1\n- route GET /page  lib/src/lib.rs:6\n\
             Most central items:\n- fn lib::store::keep  lib/src/store.rs:1\n\
             - trait lib::Show  lib/src/lib.rs:2\n- fn lib::run  lib/src/lib.rs:5\n\
             - fn lib::store::keep_more  lib/src/store.rs:2\n- fn app::main  app/src/main.rs:1\n\
             - struct lib::Page  lib/src/lib.rs:3\n- fn lib::router  lib/src/lib.rs:6\n\
             Crates:\n- app  app  1 items\n- lib  lib  6 items\n"
        );
    }

    #[test]
    fn stops_within_the_budget_and_says_what_it_left_out() {
        let full = repo().start_here(10_000);
        let short = repo().start_here(120);
        assert!(short.len() <= 120, "{short}");
        let kept = short.lines().count() - 1;
        assert_eq!(
            short.lines().take(kept).collect::<Vec<_>>(),
            full.lines().take(kept).collect::<Vec<_>>()
        );
        let left_out = full.lines().count() - kept;
        assert!(
            short.ends_with(&format!("… {left_out} more lines\n")),
            "{short}"
        );
        assert_eq!(
            repo().start_here(0),
            format!("… {} more lines\n", full.lines().count())
        );
    }

    #[test]
    fn keeps_whole_answers_that_fit_and_cuts_the_rest_before_its_tail() {
        let line = "a line of twenty-six bytes";
        let text = |budget: usize, lines: usize| {
            let mut text = Text::new(budget);
            (0..lines).for_each(|_| text.line(line));
            text.finish()
        };
        assert_eq!(text(line.len() + 1, 1), "a line of twenty-six bytes\n");
        assert_eq!(
            text(3 * (line.len() + 1), 3),
            "a line of twenty-six bytes\n".repeat(3)
        );
        let first_and_tail = line.len() + 1 + TAIL;
        assert_eq!(
            text(first_and_tail, 3),
            "a line of twenty-six bytes\n… 2 more lines\n"
        );
        assert_eq!(text(first_and_tail - 1, 2), "… 2 more lines\n");
        assert_eq!(
            text(first_and_tail, 2),
            "a line of twenty-six bytes\n… 1 more line\n"
        );
    }

    #[test]
    fn describes_the_architecture() {
        assert_eq!(
            repo().architecture(10_000),
            "Architecture: 2 crates in 1 areas.\n\
             Crates and the crates they use, with how many edges:\n- app uses lib 1\n\
             Areas, each named by its most central file, largest first:\n\
             - area:lib/src/store (3 files): lib/src/store.rs, lib/src/lib.rs, app/src/main.rs\n"
        );
    }

    #[test]
    fn ranks_hotspots_by_churn_and_what_depends_on_them() {
        let churn = [
            ("lib/src/lib.rs", 3),
            ("lib/src/store.rs", 3),
            ("README.md", 40),
            ("app/src/main.rs", 10),
        ]
        .map(|(file, commits)| Churn {
            file: file.to_owned(),
            commits,
        });
        assert_eq!(
            repo().hotspots(&churn, 10_000),
            "Hotspots: files that change often and that much depends on.\n\
             - lib/src/store.rs  3 commits, 3 edges in\n- app/src/main.rs  10 commits, 0 edges in\n\
             - lib/src/lib.rs  3 commits, 2 edges in\n"
        );
    }

    #[test]
    fn lists_contracts_of_one_kind() {
        let index = repo();
        assert_eq!(
            index.contracts(Some(ContractKind::ErrorCode), 10_000),
            "- code store.full  lib::store::keep  lib/src/store.rs:1\n"
        );
        assert_eq!(index.contracts(None, 10_000).lines().count(), 2);
    }

    #[test]
    fn tells_what_an_item_relies_on_and_what_relies_on_it() {
        assert_eq!(
            repo().item("lib::run", 10_000),
            "fn lib::run  lib/src/lib.rs:5\ncalled by fn app::main  app/src/main.rs:1\n\
             tested by lib::tests::runs\n\
             calls fn lib::store::keep  lib/src/store.rs:1\n\
             calls fn lib::store::keep_more  lib/src/store.rs:2\n"
        );
        assert_eq!(
            repo().item("lib::Page", 10_000),
            "struct lib::Page  lib/src/lib.rs:3\nimplements trait lib::Show  lib/src/lib.rs:2\n"
        );
        assert_eq!(
            repo().item("lib::store::keep", 10_000).lines().nth(1),
            Some("declares code store.full  lib/src/store.rs:1")
        );
    }

    #[test]
    fn suggests_items_for_a_path_that_names_none() {
        assert_eq!(
            repo().item("keep", 10_000),
            "No item keep.\nItems with that name:\n- fn lib::store::keep  lib/src/store.rs:1\n"
        );
        assert_eq!(repo().item("nothing", 10_000), "No item nothing.\n");
    }
}
