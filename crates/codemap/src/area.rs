use std::collections::BTreeMap;

use crate::Fingerprint;
use crate::graph::{Links, ModuleNode};
use crate::louvain::communities;
use crate::rank::{by_rank, page_rank};

/// Files that name each other more than they name the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Area {
    /// Its highest-ranked file's path without `.rs`: `crates/protocol/src/lib`. It holds while
    /// that file stays on top, and changes when another file outranks it.
    pub id: String,
    /// Highest page rank first.
    pub files: Vec<String>,
}

/// Groups the nodes' files into areas, sorted by id.
pub(crate) fn areas(nodes: &[ModuleNode<'_>], links: &Links) -> Vec<Area> {
    let mut groups: BTreeMap<usize, Vec<(&str, f64)>> = BTreeMap::new();
    let ranked = nodes.iter().zip(page_rank(links)).zip(communities(links));
    for ((node, rank), community) in ranked {
        groups
            .entry(community)
            .or_default()
            .push((&node.module.file, rank));
    }
    let mut areas: Vec<Area> = groups.into_values().map(area).collect();
    areas.sort_by(|one, other| one.id.cmp(&other.id));
    areas
}

fn area(files: Vec<(&str, f64)>) -> Area {
    let files = by_rank(files);
    let top = files.first().copied().unwrap_or_default();
    Area {
        id: top.strip_suffix(".rs").unwrap_or(top).to_owned(),
        files: files.into_iter().map(str::to_owned).collect(),
    }
}

impl Area {
    /// Changes when a file joins or leaves the area, not when its files' code changes.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut files: Vec<&str> = self.files.iter().map(String::as_str).collect();
        files.sort_unstable();
        Fingerprint::of(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::file;
    use crate::{Index, index};

    fn ids(areas: &[Area]) -> Vec<Vec<&str>> {
        areas
            .iter()
            .map(|area| {
                let mut ids = vec![area.id.as_str()];
                ids.extend(area.files.iter().map(String::as_str));
                ids
            })
            .collect()
    }

    /// Two crates whose modules use each other, joined by one path, with `extra` added to `a/src/x.rs`.
    fn repo(extra: &str) -> Index {
        index(&[
            file("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            file(
                "a/src/lib.rs",
                "mod x;\nmod y;\npub fn top() { x::f(); y::g(); }\n",
            ),
            file(
                "a/src/x.rs",
                &format!("pub fn f() {{ crate::y::g(); crate::top(); }}\n{extra}"),
            ),
            file(
                "a/src/y.rs",
                "pub fn g() { crate::x::f(); crate::top(); }\n",
            ),
            file("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            file(
                "b/src/lib.rs",
                "mod u;\nmod v;\npub fn top() { u::f(); v::g(); a::top(); }\n",
            ),
            file(
                "b/src/u.rs",
                "pub fn f() { crate::v::g(); crate::top(); }\n",
            ),
            file(
                "b/src/v.rs",
                "pub fn g() { crate::u::f(); crate::top(); }\n",
            ),
        ])
        .unwrap()
    }

    const EXPECTED: [&[&str]; 2] = [
        &["a/src/lib", "a/src/lib.rs", "a/src/x.rs", "a/src/y.rs"],
        &["b/src/lib", "b/src/lib.rs", "b/src/u.rs", "b/src/v.rs"],
    ];

    #[test]
    fn groups_files_that_use_each_other() {
        assert_eq!(ids(&repo("").areas), EXPECTED);
    }

    #[test]
    fn the_same_files_give_the_same_areas() {
        assert_eq!(repo("").areas, repo("").areas);
    }

    #[test]
    fn ids_survive_edits_that_keep_the_top_file() {
        let edited = repo("pub fn more() { let x = 2; }\n");
        assert_eq!(ids(&edited.areas), EXPECTED);
        let bridged = repo("pub fn more() { b::top(); }\n");
        assert_eq!(ids(&bridged.areas), EXPECTED);
    }

    #[test]
    fn fingerprints_follow_membership_only() {
        let before = repo("");
        let after = repo("pub fn more() { let x = 2; }\n");
        let fingerprint_of = |index: &Index| index.area("a/src/lib").unwrap().fingerprint();
        assert_eq!(fingerprint_of(&before), fingerprint_of(&after));
        assert_ne!(
            fingerprint_of(&before),
            before.area("b/src/lib").unwrap().fingerprint()
        );
        assert!(before.area("a/src/x").is_none());
    }

    #[test]
    fn files_go_by_rank_then_path() {
        let found = area(vec![
            ("b.rs", 0.5 + 1e-15),
            ("a.rs", 0.5),
            ("c", 0.7),
            ("d", 0.5 - 1e-6),
        ]);
        assert_eq!(found.id, "c");
        assert_eq!(found.files, ["c", "a.rs", "b.rs", "d"]);
    }

    #[test]
    fn no_files_make_no_areas() {
        assert!(areas(&[], &Vec::new()).is_empty());
        assert_eq!(area(Vec::new()).id, "");
    }
}
