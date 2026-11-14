use std::collections::BTreeMap;

use super::{Located, Text};
use crate::{EdgeKind, Index};

/// How many items are suggested when a path names none.
const SUGGESTED: usize = 10;

impl Index {
    /// One item: where it is, what it calls, uses and implements, what relies on it, what
    /// tests it, and the contracts it declares. A path that names no item lists the items whose
    /// path ends with it, such as `Index::check` for `weacon_codemap::anchor::Index::check`.
    #[must_use]
    pub fn item(&self, path: &str, budget: usize) -> String {
        let located = self.located();
        let mut text = Text::new(budget);
        let Some(found) = located.get(path) else {
            self.suggest(path, &located, &mut text);
            return text.finish();
        };
        text.line(&found.describe(path));
        for contract in self
            .contracts
            .iter()
            .filter(|contract| contract.item == path)
        {
            text.line(&format!(
                "declares {}  {}:{}",
                contract.describe(),
                contract.file,
                contract.line
            ));
        }
        let rank: BTreeMap<&str, usize> = (0..)
            .zip(&self.central)
            .map(|(at, known)| (known.as_str(), at))
            .collect();
        let related = self.edges.iter().filter_map(|edge| {
            if edge.from == path {
                Some((outgoing(edge.kind), edge.to.as_str()))
            } else if edge.to == path {
                Some((incoming(edge.kind), edge.from.as_str()))
            } else {
                None
            }
        });
        let mut related: Vec<(&str, &str)> = related.collect();
        related.sort_by_key(|(word, other)| {
            (
                order(word),
                rank.get(other).copied().unwrap_or(usize::MAX),
                *other,
            )
        });
        for (word, other) in related {
            let line = match located.get(other) {
                Some(found) => format!("{word} {}", found.describe(other)),
                None => format!("{word} {other}"),
            };
            text.line(&line);
        }
        text.finish()
    }
}

impl Index {
    /// Lists the items whose path ends with `path`, most central first.
    fn suggest(&self, path: &str, located: &BTreeMap<String, Located<'_>>, text: &mut Text) {
        text.line(&format!("No item {path}."));
        let suffix = format!("::{path}");
        let close = self.central.iter().filter(|known| known.ends_with(&suffix));
        let close: Vec<String> = close
            .filter_map(|known| Some(format!("- {}", located.get(known)?.describe(known))))
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

fn outgoing(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Uses => "uses",
        EdgeKind::Implements => "implements",
        EdgeKind::Tests => "tests",
    }
}

fn incoming(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "called by",
        EdgeKind::Uses => "used by",
        EdgeKind::Implements => "implemented by",
        EdgeKind::Tests => "tested by",
    }
}

/// The order the relations are listed in.
fn order(word: &str) -> usize {
    [
        "called by",
        "used by",
        "implemented by",
        "tested by",
        "calls",
        "uses",
        "implements",
        "tests",
    ]
    .iter()
    .position(|known| *known == word)
    .unwrap_or(usize::MAX)
}
