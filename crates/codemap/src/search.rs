use std::collections::BTreeMap;

use crate::Index;
use crate::facts::{Facts, FileKind};

/// What search finds: an item by its path and the contracts it declares, or a document by
/// its text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub file: String,
    /// `fn`, `struct` and the other kinds of item, or `doc`.
    pub kind: String,
    pub name: String,
    pub body: String,
}

/// Each Markdown file's text, by its path.
pub(crate) fn docs(facts: &[Facts]) -> BTreeMap<String, String> {
    facts
        .iter()
        .filter_map(|facts| match &facts.kind {
            FileKind::Doc { text, .. } => Some((facts.path.clone(), text.clone())),
            FileKind::Manifest(_) | FileKind::Rust { .. } => None,
        })
        .collect()
}

impl Index {
    /// Every item and document, for a search index.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        let mut declared: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for contract in &self.contracts {
            declared
                .entry(&contract.item)
                .or_default()
                .push(contract.describe());
        }
        let items = self.items.iter().map(|(path, found)| Entry {
            file: found.file.clone(),
            kind: found.symbol.kind.to_string(),
            name: path.clone(),
            body: declared
                .get(path.as_str())
                .map(|all| all.join("\n"))
                .unwrap_or_default(),
        });
        let docs = self.docs.iter().map(|(file, text)| Entry {
            file: file.clone(),
            kind: "doc".to_owned(),
            name: file.clone(),
            body: text.clone(),
        });
        items.chain(docs).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::test_support::files;

    #[test]
    fn lists_items_with_their_contracts_and_documents_with_their_text() {
        let index = index(&files(&[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub struct S;\nfn f() { code!(\"a.b\"); }\n",
            ),
            ("docs/a.md", "# Serving\n"),
        ]))
        .unwrap();
        let entry = |file: &str, kind: &str, name: &str, body: &str| Entry {
            file: file.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            body: body.to_owned(),
        };
        assert_eq!(
            index.entries(),
            [
                entry("a/src/lib.rs", "struct", "a::S", ""),
                entry("a/src/lib.rs", "fn", "a::f", "code a.b"),
                entry("docs/a.md", "doc", "docs/a.md", "# Serving\n"),
            ]
        );
    }
}
