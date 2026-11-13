use std::path::Path;

use serde::Deserialize;
use tree_sitter::Parser;

use crate::outline::symbols;
use crate::paths::{Named, paths};
use crate::syntax::{parse, rust_parser};
use crate::{Error, SourceFile, Symbol};

/// What one file adds to the index, found from its text alone, so a file whose text hasn't
/// changed needn't be read again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) path: String,
    pub(crate) kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileKind {
    /// A `Cargo.toml` with a package, and the name code uses for it.
    Manifest(String),
    Rust {
        symbols: Vec<Symbol>,
        named: Vec<Named>,
    },
}

/// Reads files into `Facts`, reusing one parser.
pub(crate) struct Reader {
    parser: Parser,
}

#[derive(Deserialize)]
struct Manifest {
    package: Option<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

impl Reader {
    /// # Errors
    ///
    /// When the Rust grammar can't be loaded.
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            parser: rust_parser()?,
        })
    }

    /// The facts of a `.rs` file or of a `Cargo.toml` with a package; `None` for anything else,
    /// a manifest that isn't valid included.
    ///
    /// # Errors
    ///
    /// When the parser gives up on the file.
    pub(crate) fn facts(&mut self, file: &SourceFile) -> Result<Option<Facts>, Error> {
        let name = file.path.rsplit('/').next().unwrap_or_default();
        let kind = if name == "Cargo.toml" {
            manifest(&file.text)
        } else if Path::new(name).extension().is_some_and(|it| it == "rs") {
            let tree = parse(&mut self.parser, &file.text)?;
            let source = file.text.as_bytes();
            Some(FileKind::Rust {
                symbols: symbols(&tree, source),
                named: paths(tree.root_node(), source),
            })
        } else {
            None
        };
        Ok(kind.map(|kind| Facts {
            path: file.path.clone(),
            kind,
        }))
    }
}

fn manifest(text: &str) -> Option<FileKind> {
    let manifest: Manifest = toml::from_str(text).ok()?;
    let package = manifest.package?;
    Some(FileKind::Manifest(package.name.replace('-', "_")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str, text: &str) -> Option<Facts> {
        let file = SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        };
        Reader::new().unwrap().facts(&file).unwrap()
    }

    #[test]
    fn reads_manifests_with_a_package() {
        let facts = read("a/Cargo.toml", "[package]\nname = \"b-two\"\n").unwrap();
        assert_eq!(facts.path, "a/Cargo.toml");
        assert_eq!(facts.kind, FileKind::Manifest("b_two".to_owned()));
        assert_eq!(read("Cargo.toml", "[workspace]\n"), None);
        assert_eq!(read("Cargo.toml", "[package"), None);
        assert_eq!(read("notCargo.toml", "[package]\nname = \"a\"\n"), None);
    }

    #[test]
    fn reads_rust_files_only() {
        let Some(Facts {
            kind: FileKind::Rust { symbols, named },
            ..
        }) = read("src/lib.rs", "mod x;\nfn top() {}\n")
        else {
            panic!("not read as Rust");
        };
        let names: Vec<&str> = symbols.iter().map(|it| it.name.as_str()).collect();
        assert_eq!(names, ["top"]);
        assert_eq!(named.len(), 1);
        assert_eq!(read("README.md", "fn top() {}"), None);
        assert_eq!(read("src/lib.rs.bak", "fn top() {}"), None);
    }
}
