use std::collections::BTreeMap;

use super::Text;
use crate::{EdgeKind, Index};

/// How many items are suggested when a path names none.
const SUGGESTED: usize = 10;

impl Index {
    /// One item: where it is, what relies on it and tests it, what it calls, uses and
    /// implements, and the contracts it declares. A path that names no item lists the items
    /// whose path ends with it, such as `Index::check` for `weacon_codemap::anchor::Index::check`.
    #[must_use]
    pub fn item(&self, path: &str, budget: usize) -> String {
        let mut text = Text::new(budget);
        let Some(found) = self.items.get(path) else {
            self.suggest(path, &mut text);
            return text.finish();
        };
        text.line(&found.describe(path));
        let declared = self
            .contracts
            .iter()
            .filter(|contract| contract.item == path);
        for contract in declared {
            let at = format!("{}:{}", contract.file, contract.line);
            text.line(&format!("declares {}  {at}", contract.describe()));
        }
        let rank: BTreeMap<&str, usize> = (0..)
            .zip(&self.central)
            .map(|(at, known)| (known.as_str(), at))
            .collect();
        let related = self.edges.iter().filter_map(|edge| {
            if edge.from == path {
                Some((true, edge.kind, edge.to.as_str()))
            } else if edge.to == path {
                Some((false, edge.kind, edge.from.as_str()))
            } else {
                None
            }
        });
        let mut related: Vec<(bool, EdgeKind, &str)> = related.collect();
        related.sort_by_key(|&(outgoing, kind, other)| {
            let at = rank.get(other).copied().unwrap_or(usize::MAX);
            (outgoing, kind, at, other)
        });
        for (outgoing, kind, other) in related {
            let word = relation(outgoing, kind);
            let line = match self.items.get(other) {
                Some(found) => format!("{word} {}", found.describe(other)),
                None => format!("{word} {other}"),
            };
            text.line(&line);
        }
        text.finish()
    }

    /// Lists the items whose path ends with `path`, most central first.
    fn suggest(&self, path: &str, text: &mut Text) {
        text.line(&format!("No item {path}."));
        let suffix = format!("::{path}");
        let close: Vec<String> = self
            .central
            .iter()
            .filter(|known| known.ends_with(&suffix))
            .filter_map(|known| Some(format!("- {}", self.items.get(known)?.describe(known))))
            .take(SUGGESTED)
            .collect();
        if !close.is_empty() {
            text.line("Items with that name:");
        }
        for line in close {
            text.line(&line);
        }
    }
}

fn relation(outgoing: bool, kind: EdgeKind) -> &'static str {
    match (outgoing, kind) {
        (true, EdgeKind::Calls) => "calls",
        (true, EdgeKind::Uses) => "uses",
        (true, EdgeKind::Implements) => "implements",
        (true, EdgeKind::Tests) => "tests",
        (false, EdgeKind::Calls) => "called by",
        (false, EdgeKind::Uses) => "used by",
        (false, EdgeKind::Implements) => "implemented by",
        (false, EdgeKind::Tests) => "tested by",
    }
}
