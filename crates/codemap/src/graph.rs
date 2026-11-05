use std::collections::{BTreeMap, BTreeSet};

use crate::paths::Named;
use crate::{Crate, Module};

/// Weighted links from each node to others, by node number.
pub(crate) type Links = Vec<BTreeMap<usize, f64>>;

/// A graph node: a module and its crate.
pub(crate) type Node<'a> = (&'a Crate, &'a Module);

/// The modules in index order, numbered as graph nodes.
pub(crate) fn nodes(crates: &[Crate]) -> Vec<Node<'_>> {
    crates
        .iter()
        .flat_map(|krate| krate.modules.iter().map(move |module| (krate, module)))
        .collect()
}

/// A link from each file to each other file it names, weighted by how often it names it.
pub(crate) fn file_links(nodes: &[Node<'_>], named: &BTreeMap<String, Vec<Named>>) -> Links {
    let numbers: BTreeMap<(&str, &str), usize> = nodes
        .iter()
        .enumerate()
        .map(|(number, (krate, module))| ((krate.name.as_str(), module.path.as_str()), number))
        .collect();
    let crate_names: BTreeSet<&str> = nodes.iter().map(|(krate, _)| krate.name.as_str()).collect();
    let deepest = nodes
        .iter()
        .map(|(_, module)| depth(&module.path))
        .max()
        .unwrap_or(0);
    nodes
        .iter()
        .enumerate()
        .map(|(number, (krate, module))| {
            let targets = named
                .get(&module.file)
                .into_iter()
                .flatten()
                .filter_map(|path| {
                    let (owner, segments) = absolute(&crate_names, &krate.name, &module.path, path);
                    longest_module(&numbers, owner, &segments, deepest)
                });
            let mut weights = BTreeMap::new();
            for target in targets.filter(|target| *target != number) {
                *weights.entry(target).or_insert(0.0) += 1.0;
            }
            weights
        })
        .collect()
}

/// The crate a path starts in, and its segments from that crate's root.
fn absolute<'a>(
    crates: &BTreeSet<&str>,
    krate: &'a str,
    module: &'a str,
    path: &'a Named,
) -> (&'a str, Vec<&'a str>) {
    let mut here: Vec<&str> = module.split("::").filter(|it| !it.is_empty()).collect();
    here.extend(path.within.iter().map(String::as_str));
    let mut segments = path.segments.iter().map(String::as_str).peekable();
    match segments.peek().copied() {
        Some("crate") => {
            segments.next();
            here.clear();
        }
        Some("self") => {
            segments.next();
        }
        Some("super") => {
            while segments.next_if_eq(&"super").is_some() {
                here.pop();
            }
        }
        Some(name) if crates.contains(name) => {
            segments.next();
            return (name, segments.collect());
        }
        _ => {}
    }
    here.extend(segments);
    (krate, here)
}

fn depth(module: &str) -> usize {
    module.split("::").filter(|it| !it.is_empty()).count()
}

/// The deepest module that `segments` passes through, if any; no module is deeper than `deepest`.
fn longest_module(
    numbers: &BTreeMap<(&str, &str), usize>,
    krate: &str,
    segments: &[&str],
    deepest: usize,
) -> Option<usize> {
    (0..=segments.len().min(deepest)).rev().find_map(|length| {
        let module = segments.get(..length)?.join("::");
        numbers.get(&(krate, module.as_str())).copied()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(path: &str, file: &str) -> Module {
        Module {
            path: path.to_owned(),
            file: file.to_owned(),
            symbols: Vec::new(),
        }
    }

    fn krate(name: &str, modules: Vec<Module>) -> Crate {
        Crate {
            name: name.to_owned(),
            folder: name.to_owned(),
            modules,
        }
    }

    /// `inner/super::y` is `super::y` written inside the inline module `inner`.
    fn path(text: &str) -> Named {
        let (within, segments) = text.rsplit_once('/').unwrap_or(("", text));
        Named {
            within: within
                .split('/')
                .filter(|it| !it.is_empty())
                .map(str::to_owned)
                .collect(),
            segments: segments.split("::").map(str::to_owned).collect(),
        }
    }

    fn links(named: &[(&str, &[&str])]) -> Vec<Vec<(usize, f64)>> {
        let crates = [
            krate(
                "a",
                vec![
                    module("", "a.rs"),
                    module("x", "a/x.rs"),
                    module("x::y", "a/x/y.rs"),
                ],
            ),
            krate("b", vec![module("", "b.rs")]),
        ];
        let paths = named
            .iter()
            .map(|(file, paths)| {
                (
                    (*file).to_owned(),
                    paths.iter().map(|it| path(it)).collect(),
                )
            })
            .collect();
        file_links(&nodes(&crates), &paths)
            .into_iter()
            .map(|targets| targets.into_iter().collect())
            .collect()
    }

    #[test]
    fn resolves_paths_from_the_crate_root() {
        let found = links(&[(
            "a.rs",
            &["crate::x::y::f", "x::Thing", "b::B", "std::fs::read"],
        )]);
        assert_eq!(
            found,
            [vec![(1, 1.0), (2, 1.0), (3, 1.0)], vec![], vec![], vec![]]
        );
    }

    #[test]
    fn resolves_paths_relative_to_a_module() {
        let found = links(&[
            (
                "a/x.rs",
                &["self::y::f", "y::g", "super::h", "super::x::y::G"],
            ),
            ("a/x/y.rs", &["super::super::i", "super::j", "crate::k"]),
        ]);
        assert_eq!(
            found,
            [
                vec![],
                vec![(0, 1.0), (2, 3.0)],
                vec![(0, 2.0), (1, 1.0)],
                vec![]
            ]
        );
    }

    #[test]
    fn counts_repeated_names_and_skips_a_file_naming_itself() {
        let found = links(&[("b.rs", &["a::x", "a::x::f", "self::g", "crate::B", "b::B"])]);
        assert_eq!(found, [vec![], vec![], vec![], vec![(1, 2.0)]]);
    }

    #[test]
    fn resolves_paths_inside_inline_modules_from_there() {
        let found = links(&[(
            "a/x.rs",
            &[
                "inner/super::y::f",
                "inner/self::g",
                "inner/deeper/super::super::super::h",
            ],
        )]);
        assert_eq!(found, [vec![], vec![(0, 1.0), (2, 1.0)], vec![], vec![]]);
    }

    #[test]
    fn a_long_path_resolves_through_the_deepest_module() {
        let long = format!("crate::x::y{}", "::z".repeat(20_000));
        assert_eq!(
            links(&[("b.rs", &[&format!("a::x::y{}", "::z".repeat(20_000))])])[3],
            [(2, 1.0)]
        );
        assert_eq!(links(&[("a.rs", &[&long])])[0], [(2, 1.0)]);
    }

    #[test]
    fn a_path_through_no_module_names_the_root() {
        let found = links(&[("a/x/y.rs", &["crate::nothing::here"])]);
        assert_eq!(found, [vec![], vec![], vec![(0, 1.0)], vec![]]);
    }
}
