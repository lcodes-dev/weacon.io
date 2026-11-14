mod commands;
mod tables;

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use commands::{Commands, Variant, full_names, variants};
use tables::Tables;

use crate::edges::id;
use crate::graph::nodes;
use crate::outline::Item;
use crate::paths::segments;
use crate::syntax::{attributes, line, preorder, text};
use crate::{Crate, Module};

/// Something other programs or people rely on staying as it is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Contract {
    pub kind: ContractKind,
    /// `map check-anchors`, `store.full`, `GET /version`, `workspaces`, or a type's path.
    pub name: String,
    /// The path of the item that declares it.
    pub item: String,
    pub file: String,
    /// Counting from 1.
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContractKind {
    /// A subcommand, after the subcommands above it.
    Command,
    /// A code given to `code!`.
    ErrorCode,
    /// A method and a path, or only a path for a route that takes any method.
    Route,
    /// A type that derives `Serialize` or `Deserialize`.
    Serde,
    Table,
}

/// A contract an item declares, as found in its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declared {
    pub(crate) kind: Declares,
    pub(crate) line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Declares {
    /// The item itself.
    Serde,
    ErrorCode(String),
    Route(String),
    Table(String),
    /// A variant of an enum that derives `Subcommand`.
    Command(Variant),
}

const METHODS: &[&str] = &["delete", "get", "head", "options", "patch", "post", "put"];

/// Every contract the crates' items declare, sorted, each once.
pub(crate) fn assemble(crates: &[Crate], items: &BTreeMap<&str, &[Item]>) -> Vec<Contract> {
    let mut found = BTreeSet::new();
    let mut commands = Commands::new();
    for (krate, module) in nodes(crates) {
        for item in items.get(module.file.as_str()).copied().unwrap_or_default() {
            declare(&mut found, &mut commands, krate, module, item);
        }
    }
    found.extend(full_names(&commands));
    found.into_iter().collect()
}

/// Adds what `item` declares to `found`, and its subcommands to `commands`.
fn declare<'a>(
    found: &mut BTreeSet<Contract>,
    commands: &mut Commands<'a>,
    krate: &'a Crate,
    module: &Module,
    item: &'a Item,
) {
    let path = id(&krate.name, &module.path, &item.symbol.name);
    for declared in &item.contracts {
        let contract = |kind, name| Contract {
            kind,
            name,
            item: path.clone(),
            file: module.file.clone(),
            line: declared.line,
        };
        let (kind, name) = match &declared.kind {
            Declares::Serde => (ContractKind::Serde, path.clone()),
            Declares::ErrorCode(code) => (ContractKind::ErrorCode, code.clone()),
            Declares::Route(route) => (ContractKind::Route, route.clone()),
            Declares::Table(table) => (ContractKind::Table, table.clone()),
            Declares::Command(variant) => {
                let key = (krate.name.as_str(), last(&item.symbol.name));
                let command = contract(ContractKind::Command, variant.name.clone());
                commands.entry(key).or_default().push((command, variant));
                continue;
            }
        };
        found.insert(contract(kind, name));
    }
}

fn last(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or_default()
}

/// The contracts `item` declares, in source order.
pub(crate) fn contracts(item: Node<'_>, source: &[u8]) -> Vec<Declared> {
    let derives = derives(item, source);
    let mut found = Vec::new();
    if derives
        .iter()
        .any(|it| matches!(*it, "Serialize" | "Deserialize"))
    {
        found.push(declared(item, Declares::Serde));
    }
    if derives.contains(&"Subcommand") && item.kind() == "enum_item" {
        found.extend(variants(item, source));
    }
    let mut tables = Tables::default();
    preorder(item, |node| {
        match node.kind() {
            "macro_invocation" => found.extend(error_code(node, source)),
            "call_expression" => found.extend(routes(node, source)),
            "string_literal" | "raw_string_literal" => {
                let sql = literal(node, source);
                for (number, text) in (line(node)..).zip(sql.lines()) {
                    tables.apply(text, number);
                }
            }
            _ => {}
        }
        true
    });
    found.extend(tables.into_lines().map(|(table, line)| Declared {
        kind: Declares::Table(table),
        line,
    }));
    found
}

pub(crate) fn declared(node: Node<'_>, kind: Declares) -> Declared {
    Declared {
        kind,
        line: line(node),
    }
}

/// The names in the `#[derive(...)]`s on `item`.
fn derives<'a>(item: Node<'_>, source: &'a [u8]) -> Vec<&'a str> {
    attributes(item)
        .filter_map(|node| {
            let attribute = node.named_child(0)?;
            let name = attribute.named_child(0)?;
            (text(name, source) == "derive").then_some(attribute.child_by_field_name("arguments")?)
        })
        .flat_map(|arguments| {
            text(arguments, source)
                .split(|it: char| !(it.is_alphanumeric() || it == '_' || it == ':'))
                .map(last)
                .filter(|name| !name.is_empty())
        })
        .collect()
}

fn error_code(node: Node<'_>, source: &[u8]) -> Option<Declared> {
    let name = segments(node.child_by_field_name("macro")?, source)?;
    if name.last()? != "code" {
        return None;
    }
    let mut cursor = node.walk();
    let tokens = node
        .named_children(&mut cursor)
        .find(|it| it.kind() == "token_tree")?;
    let mut cursor = tokens.walk();
    let code = tokens
        .named_children(&mut cursor)
        .find(|it| it.kind() == "string_literal")?;
    Some(declared(node, Declares::ErrorCode(literal(code, source))))
}

/// The routes a `.route(path, handlers)` call declares, one per method, on the line of `route`.
fn routes(node: Node<'_>, source: &[u8]) -> Vec<Declared> {
    let Some(field) = node
        .child_by_field_name("function")
        .and_then(|function| function.child_by_field_name("field"))
        .filter(|field| text(*field, source) == "route")
    else {
        return Vec::new();
    };
    let arguments = node.child_by_field_name("arguments");
    let Some(path) = arguments
        .and_then(|it| it.named_child(0))
        .filter(|it| it.kind() == "string_literal")
    else {
        return Vec::new();
    };
    let path = literal(path, source);
    let mut methods = Vec::new();
    if let Some(handlers) = arguments.and_then(|it| it.named_child(1)) {
        preorder(handlers, |node| {
            let name = (node.kind() == "call_expression")
                .then(|| called(node, source))
                .flatten()
                .filter(|name| METHODS.contains(name));
            methods.extend(name.map(|name| format!("{} {path}", name.to_uppercase())));
            true
        });
    }
    if methods.is_empty() {
        methods.push(path);
    }
    methods
        .into_iter()
        .map(|route| declared(field, Declares::Route(route)))
        .collect()
}

/// The name a call calls: `get` for `get(h)`, `routing::get(h)` or `x.get(h)`.
fn called<'a>(call: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let function = call.child_by_field_name("function")?;
    let name = match function.kind() {
        "identifier" => function,
        "scoped_identifier" => function.child_by_field_name("name")?,
        "field_expression" => function.child_by_field_name("field")?,
        _ => return None,
    };
    Some(text(name, source))
}

/// A string literal's text, escapes left out.
pub(crate) fn literal(node: Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|it| it.kind() == "string_content")
        .map(|it| text(it, source))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{SourceFile, index};

    fn found(lib: &str) -> Vec<String> {
        let files = [
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("a/src/lib.rs", lib),
        ]
        .map(|(path, text)| SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        });
        index(&files)
            .unwrap()
            .contracts
            .into_iter()
            .map(|it| format!("{:?} {} @{}:{}", it.kind, it.name, it.item, it.line))
            .collect()
    }

    #[test]
    fn finds_serde_types_codes_routes_and_tables() {
        let lib = "#[derive(Debug, serde::Serialize)]\n/// A doc.\npub struct Hello;\n\
                   #[derive(Clone)]\npub struct Plain;\n\
                   fn fail() { Fault::new(code!(\"store.full\"), \"Full\"); weacon::code!(\"a.b\"); other!(\"c.d\"); }\n\
                   fn router() -> Router { Router::new()\n.route(\"/version\", get(version))\n\
                   .route(\"/tasks\", get(list).post(tasks::create))\n.route(\"/web\", any) }\n\
                   const STEPS: &[&str] = &[\"\ncreate table if not exists notes (id INTEGER);\n\
                   CREATE VIRTUAL TABLE found USING fts5(text); CREATE INDEX x ON notes (id);\"];\n";
        assert_eq!(
            found(lib),
            [
                "ErrorCode a.b @a::fail:6",
                "ErrorCode store.full @a::fail:6",
                "Route /web @a::router:10",
                "Route GET /tasks @a::router:9",
                "Route GET /version @a::router:8",
                "Route POST /tasks @a::router:9",
                "Serde a::Hello @a::Hello:3",
                "Table found @a::STEPS:13",
                "Table notes @a::STEPS:12",
            ]
        );
    }

    #[test]
    fn honours_flatten_names_and_loops() {
        let lib = "#[derive(Subcommand)]\nenum Top { #[command(flatten)] Admin(Admin), \
                   #[command(about = \"Again\", name = \"go-on\")] Again(Again) }\n\
                   #[derive(Subcommand)]\nenum Admin { AddMember }\n\
                   #[derive(Subcommand)]\nenum Again { More(Again) }\n\
                   #[derive(Subcommand)]\nstruct NotAnEnum;\n\
                   #[derive(Subcommand, other::Serialize)]\nenum Quiet { Quietly }\n\
                   #[derive(Not_Serialize)]\nenum Plain { Hidden }\n";
        let found = found(lib);
        let depth = found.iter().map(|it| it.matches(" more").count()).max();
        assert_eq!(depth, Some(7), "{found:?}");
        assert_eq!(
            found
                .iter()
                .filter(|it| !it.contains(" more"))
                .collect::<Vec<_>>(),
            [
                "Command add-member @a::Admin:4",
                "Command go-on @a::Top:2",
                "Command quietly @a::Quiet:10",
                "Serde a::Quiet @a::Quiet:10",
            ]
        );
    }

    #[test]
    fn reads_methods_however_they_are_written() {
        let lib = "fn router() -> Router { Router::new()\n.route(\"/a\", routing::get(h).put(i))\n\
                   .route(\"/b\", any(get))\n.route(\"/c\", axum::routing::post(h)) }\n";
        assert_eq!(
            found(lib),
            [
                "Route /b @a::router:3",
                "Route GET /a @a::router:2",
                "Route POST /c @a::router:4",
                "Route PUT /a @a::router:2",
            ]
        );
    }

    #[test]
    fn names_subcommands_after_the_ones_above_them() {
        let lib = "#[derive(Subcommand)]\nenum Command { Daemon, #[command(subcommand)] Map(MapCommand), \
                   Login { agent: String } }\n#[derive(clap::Subcommand)]\nenum MapCommand { CheckAnchors { root: u8 }, \
                   Deep(Deeper) }\n#[derive(Subcommand)]\nenum Deeper { StartHere }\n\
                   #[derive(Subcommand)]\nenum Loop { Again(Loop) }\n";
        assert_eq!(
            found(lib),
            [
                "Command daemon @a::Command:2",
                "Command login @a::Command:2",
                "Command map @a::Command:2",
                "Command map check-anchors @a::MapCommand:4",
                "Command map deep @a::MapCommand:4",
                "Command map deep start-here @a::Deeper:6",
            ]
        );
    }
}
