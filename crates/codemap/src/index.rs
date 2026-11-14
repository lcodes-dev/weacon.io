use std::collections::BTreeMap;
use std::fmt;

use crate::area::areas;
use crate::contracts::{self, Contract};
use crate::edges::{Edge, edges};
use crate::facts::{Facts, FileKind, Reader};
use crate::flows::{self, Flow};
use crate::graph::{file_links, nodes};
use crate::rank::central;
use crate::{Area, Error, Symbol};

/// A file as the caller read it, its path relative to the repository root with `/` separators.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SourceFile").finish_non_exhaustive()
    }
}

/// The outline of a repository: its crates, their modules and the modules' items, and its
/// files grouped into areas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// Sorted by folder.
    pub(crate) crates: Vec<Crate>,
    /// Sorted by id.
    pub(crate) areas: Vec<Area>,
    /// Sorted.
    pub(crate) edges: Vec<Edge>,
    /// Sorted.
    pub(crate) contracts: Vec<Contract>,
    /// Sorted by file and line.
    pub(crate) flows: Vec<Flow>,
    /// Every item's path, most central first.
    pub(crate) central: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Crate {
    /// The name code uses, `weacon_protocol` for the package `weacon-protocol`.
    pub(crate) name: String,
    /// The folder holding `Cargo.toml`, empty at the repository root.
    pub(crate) folder: String,
    /// One per file under `src/`, sorted by file.
    pub(crate) modules: Vec<Module>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Module {
    /// Within the crate, `frame::codec`; empty for the crate's root.
    pub(crate) path: String,
    pub(crate) file: String,
    pub(crate) symbols: Vec<Symbol>,
}

/// Outlines every crate whose `Cargo.toml` is among `files`, from the `.rs` files under its `src/`.
/// A `Cargo.toml` that isn't valid is skipped.
///
/// # Errors
///
/// When the Rust grammar can't be loaded or the parser gives up on a `.rs` file.
pub fn index(files: &[SourceFile]) -> Result<Index, Error> {
    let mut reader = Reader::new()?;
    let facts = files
        .iter()
        .filter_map(|file| reader.facts(file).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(assemble(&facts))
}

/// The index of a repository from the facts of its files.
#[must_use]
pub(crate) fn assemble(facts: &[Facts]) -> Index {
    let mut crates: Vec<Crate> = facts.iter().filter_map(manifest).collect();
    crates.sort_by(|one, other| one.folder.cmp(&other.folder));
    let mut named_by_file = BTreeMap::new();
    let mut outlines = BTreeMap::new();
    let mut sources: Vec<&Facts> = facts.iter().collect();
    sources.sort_by_key(|facts| &facts.path);
    for facts in sources {
        let FileKind::Rust {
            outline,
            named_paths,
        } = &facts.kind
        else {
            continue;
        };
        let file = facts.path.as_str();
        // A folder sorts before the folders inside it, so the last match is the innermost crate.
        let Some((owner, path)) = crates
            .iter_mut()
            .rev()
            .find_map(|krate| module_path(&krate.folder, file).map(|path| (krate, path)))
        else {
            continue;
        };
        named_by_file.insert(file, &named_paths[..]);
        outlines.insert(file, outline);
        owner.modules.push(Module {
            path,
            file: file.to_owned(),
            symbols: outline
                .items
                .iter()
                .map(|item| item.symbol.clone())
                .collect(),
        });
    }
    crates.iter_mut().for_each(root_binary);
    let nodes = nodes(&crates);
    let areas = areas(&nodes, &file_links(&nodes, &named_by_file));
    let edges = edges(&crates, &named_by_file, &outlines);
    let contracts = contracts::assemble(&crates, &outlines);
    let central = central(&nodes, &edges);
    let flows = flows::check(facts, &central, &edges);
    Index {
        crates,
        areas,
        edges,
        contracts,
        flows,
        central,
    }
}

fn manifest(facts: &Facts) -> Option<Crate> {
    let FileKind::Manifest(name) = &facts.kind else {
        return None;
    };
    let folder = facts.path.strip_suffix("/Cargo.toml").unwrap_or_default();
    Some(Crate {
        name: name.clone(),
        folder: folder.to_owned(),
        modules: Vec::new(),
    })
}

/// A crate with no `lib.rs` is rooted at its `main.rs`.
fn root_binary(krate: &mut Crate) {
    if krate.modules.iter().any(|module| module.path.is_empty()) {
        return;
    }
    if let Some(main) = krate
        .modules
        .iter_mut()
        .find(|module| module.path == "main")
    {
        main.path.clear();
    }
}

/// `frame::codec` for `<folder>/src/frame/codec.rs`; `None` for files outside the crate's `src/`.
/// Binaries beside a library read as its modules: `main`, `bin::tool`.
fn module_path(folder: &str, file: &str) -> Option<String> {
    let inside = if folder.is_empty() {
        Some(file)
    } else {
        file.strip_prefix(folder)?.strip_prefix('/')
    };
    let relative = inside?.strip_prefix("src/")?.strip_suffix(".rs")?;
    let relative = relative.strip_suffix("/mod").unwrap_or(relative);
    Some(match relative {
        "lib" => String::new(),
        _ => relative.replace('/', "::"),
    })
}

impl Index {
    /// The item at `crate_name::module::path::Name`.
    #[must_use]
    pub(crate) fn symbol(&self, path: &str) -> Option<&Symbol> {
        let (name, rest) = path.split_once("::")?;
        let krate = self.crates.iter().find(|krate| krate.name == name)?;
        krate.modules.iter().find_map(|module| {
            let name = match module.path.as_str() {
                "" => Some(rest),
                prefix => rest.strip_prefix(prefix)?.strip_prefix("::"),
            }?;
            module.symbols.iter().find(|symbol| symbol.name == name)
        })
    }

    #[must_use]
    pub(crate) fn area(&self, id: &str) -> Option<&Area> {
        self.areas.iter().find(|area| area.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::file;

    fn layout(index: &Index) -> Vec<String> {
        index
            .crates
            .iter()
            .flat_map(|krate| {
                krate.modules.iter().map(move |module| {
                    let names: Vec<&str> = module
                        .symbols
                        .iter()
                        .map(|symbol| symbol.name.as_str())
                        .collect();
                    format!(
                        "{} [{}] {} {}: {}",
                        krate.name,
                        krate.folder,
                        module.path,
                        module.file,
                        names.join(",")
                    )
                })
            })
            .collect()
    }

    #[test]
    fn groups_files_into_crates_and_modules() {
        let index = index(&[
            file("Cargo.toml", "[workspace]\nmembers = []\n"),
            file("crates/b-two/Cargo.toml", "[package]\nname = \"b-two\"\n"),
            file("crates/b-two/src/lib.rs", "mod x;\nfn top() {}\n"),
            file("crates/b-two/src/x.rs", "fn inner() {}\n"),
            file("crates/b-two/src/x/y/mod.rs", "struct Deep;\n"),
            file("crates/b-two/tests/it.rs", "fn ignored() {}\n"),
            file("crates/b-two/README.md", "fn ignored() {}\n"),
            file("apps/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            file("apps/a/src/main.rs", "fn main() {}\n"),
            file("loose.rs", "fn ignored() {}\n"),
        ])
        .unwrap();

        assert_eq!(
            layout(&index),
            [
                "a [apps/a]  apps/a/src/main.rs: main",
                "b_two [crates/b-two]  crates/b-two/src/lib.rs: top",
                "b_two [crates/b-two] x crates/b-two/src/x.rs: inner",
                "b_two [crates/b-two] x::y crates/b-two/src/x/y/mod.rs: Deep",
            ]
        );
    }

    #[test]
    fn a_nested_crate_owns_its_own_files() {
        let index = index(&[
            file("Cargo.toml", "[package]\nname = \"outer\"\n"),
            file("src/lib.rs", "fn outer() {}\n"),
            file("src/inner/Cargo.toml", "[package]\nname = \"inner\"\n"),
            file("src/inner/src/lib.rs", "fn inner() {}\n"),
        ])
        .unwrap();

        assert_eq!(
            layout(&index),
            [
                "outer []  src/lib.rs: outer",
                "inner [src/inner]  src/inner/src/lib.rs: inner"
            ]
        );
    }

    #[test]
    fn only_files_named_cargo_toml_are_manifests() {
        let index = index(&[
            file("notCargo.toml", "[package]\nname = \"no\"\n"),
            file("Cargo.toml.bak", "[package]\nname = \"no\"\n"),
            file("x/notCargo.toml", "[package]\nname = \"no\"\n"),
        ])
        .unwrap();
        assert!(index.crates.is_empty());
    }

    #[test]
    fn skips_a_broken_manifest() {
        let index = index(&[
            file("a/Cargo.toml", "[package"),
            file("a/src/lib.rs", "fn a() {}\n"),
            file("b/Cargo.toml", "[package]\nversion = \"1\"\n"),
            file("c/Cargo.toml", "[package]\nname = \"c\"\n"),
            file("c/src/lib.rs", "fn c() {}\n"),
        ])
        .unwrap();
        assert_eq!(layout(&index), ["c [c]  c/src/lib.rs: c"]);
    }

    #[test]
    fn roots_a_crate_at_its_library_then_its_binary() {
        let index = index(&[
            file("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            file("a/src/lib.rs", "fn run() {}\n"),
            file("a/src/main.rs", "fn main() {}\n"),
            file("a/src/bin/tool.rs", "fn main() {}\n"),
            file("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            file("b/src/main.rs", "fn main() {}\n"),
            file("b/src/main/x.rs", "fn x() {}\n"),
        ])
        .unwrap();
        assert_eq!(
            layout(&index),
            [
                "a [a] bin::tool a/src/bin/tool.rs: main",
                "a [a]  a/src/lib.rs: run",
                "a [a] main a/src/main.rs: main",
                "b [b]  b/src/main.rs: main",
                "b [b] main::x b/src/main/x.rs: x",
            ]
        );
    }

    #[test]
    fn assembles_kept_facts_with_a_changed_file() {
        let files = [
            file("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            file("a/src/lib.rs", "mod m;\npub fn run() { m::go(); }\n"),
            file("a/src/m.rs", "pub fn go() {}\n"),
        ];
        let mut reader = Reader::new().unwrap();
        let mut facts: Vec<Facts> = files
            .iter()
            .filter_map(|file| reader.facts(file).unwrap())
            .collect();
        let changed = file("a/src/m.rs", "pub fn went() {}\n");
        facts.retain(|kept| kept.path != changed.path);
        facts.extend(reader.facts(&changed).unwrap());

        let fresh = index(&[files[0].clone(), files[1].clone(), changed]).unwrap();
        assert_eq!(assemble(&facts), fresh);
        assert!(fresh.symbol("a::m::went").is_some());
        assert_eq!(fresh.areas.len(), 1);
    }

    #[test]
    fn debug_leaves_out_file_contents() {
        let debug = format!("{:?}", file("a.rs", "secret marker"));
        assert_eq!(debug, "SourceFile { .. }");
    }

    #[test]
    fn a_crate_folder_is_matched_whole() {
        assert_eq!(module_path("crates/a", "crates/ab/src/lib.rs"), None);
        assert_eq!(
            module_path("crates/a", "crates/a/src/lib.rs"),
            Some(String::new())
        );
        assert_eq!(module_path("", "src/a/b.rs"), Some("a::b".to_owned()));
        assert_eq!(module_path("", "src/a.txt"), None);
    }
}
