use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::Parser;

use crate::flows::written;
use crate::paths::{NamedPath, named_paths};
use crate::symbols::{Outline, symbols};
use crate::syntax::{parse, rust_parser};
use crate::{Error, SourceFile};

/// What one file adds to the index, found from its text alone, so a file whose text hasn't
/// changed needn't be read again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub(crate) path: String,
    pub(crate) kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FileKind {
    /// A `Cargo.toml` with a package, and the name code uses for it.
    Manifest(String),
    Rust {
        outline: Outline,
        named_paths: Vec<NamedPath>,
    },
    /// A Markdown file: the flows it pins with their lines, and its text for search.
    Doc {
        flows: Vec<(Vec<String>, usize)>,
        text: String,
    },
}

/// Reads files into `Facts`, reusing one parser.
pub struct Reader {
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
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            parser: rust_parser()?,
        })
    }

    /// The facts of a `.rs` or `.md` file or of a `Cargo.toml` with a package; `None` for anything else,
    /// a manifest that isn't valid included.
    ///
    /// # Errors
    ///
    /// When the parser gives up on the file.
    pub fn facts(&mut self, file: &SourceFile) -> Result<Option<Facts>, Error> {
        let name = file.path.rsplit('/').next().unwrap_or_default();
        let extension = Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str());
        let kind = if name == "Cargo.toml" {
            manifest(&file.text)
        } else if extension == Some("md") {
            Some(FileKind::Doc {
                flows: written(&file.text),
                text: file.text.clone(),
            })
        } else if extension == Some("rs") {
            let tree = parse(&mut self.parser, &file.text)?;
            let source = file.text.as_bytes();
            Some(FileKind::Rust {
                outline: symbols(&tree, source),
                named_paths: named_paths(tree.root_node(), source),
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

impl Facts {
    /// The file's path, relative to the repository's root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
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
    use crate::test_support::file;

    fn read(path: &str, text: &str) -> Option<Facts> {
        Reader::new().unwrap().facts(&file(path, text)).unwrap()
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
            kind:
                FileKind::Rust {
                    outline,
                    named_paths,
                },
            ..
        }) = read("src/lib.rs", "mod x;\nfn top() {}\n")
        else {
            panic!("not read as Rust");
        };
        let names: Vec<&str> = outline
            .items
            .iter()
            .map(|item| item.symbol.name.as_str())
            .collect();
        assert_eq!(names, ["top"]);
        assert_eq!(named_paths.len(), 1);
        assert_eq!(read("notes.txt", "fn top() {}"), None);
        let doc = read("docs/a.md", "See `flow:a::run->a::serve`.\n").unwrap();
        assert_eq!(
            doc.kind,
            FileKind::Doc {
                flows: vec![(vec!["a::run".to_owned(), "a::serve".to_owned()], 1)],
                text: "See `flow:a::run->a::serve`.\n".to_owned(),
            }
        );
        assert_eq!(read("src/lib.rs.bak", "fn top() {}"), None);
    }

    #[test]
    fn round_trips_through_json() {
        let source = "pub trait T {}\npub struct S;\nimpl T for S {}\n#[derive(Serialize)]\nstruct M;\n\
                      fn f() { code!(\"a.b\"); g(); x.h(); S; }\n#[cfg(test)]\nmod tests { #[test]\n fn t() {} }\n";
        for facts in [
            read("src/lib.rs", source),
            read("Cargo.toml", "[package]\nname = \"a\"\n"),
            read("docs/a.md", "flow:a::f->a::g\n"),
        ] {
            let facts = facts.unwrap();
            let json = serde_json::to_string(&facts).unwrap();
            assert_eq!(serde_json::from_str::<Facts>(&json).unwrap(), facts);
        }
    }
}
