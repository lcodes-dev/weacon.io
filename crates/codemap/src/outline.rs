use std::collections::BTreeMap;
use std::fmt;

use tree_sitter::{Node, Parser};

use crate::{Error, Fingerprint};

/// An item declared in a module, named relative to it: `Frame`, `Frame::new`, `tests::helper`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub kind: Kind,
    pub name: String,
    /// The first line, counting from 1.
    pub line: usize,
    /// Changes when the item's code does, but not its comments, attributes or layout.
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Const,
    Enum,
    Fn,
    Macro,
    Mod,
    Static,
    Struct,
    Trait,
    Type,
    Union,
}

impl Kind {
    fn of(node: &str) -> Option<Self> {
        Some(match node {
            "const_item" => Self::Const,
            "enum_item" => Self::Enum,
            "function_item" | "function_signature_item" => Self::Fn,
            "macro_definition" => Self::Macro,
            "mod_item" => Self::Mod,
            "static_item" => Self::Static,
            "struct_item" => Self::Struct,
            "trait_item" => Self::Trait,
            "type_item" => Self::Type,
            "union_item" => Self::Union,
            _ => return None,
        })
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Const => "const",
            Self::Enum => "enum",
            Self::Fn => "fn",
            Self::Macro => "macro",
            Self::Mod => "mod",
            Self::Static => "static",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Type => "type",
            Self::Union => "union",
        })
    }
}

pub(crate) fn rust_parser() -> Result<Parser, Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|_| Error::Grammar)?;
    Ok(parser)
}

/// The items of a file, test-only items left out, in source order.
pub(crate) fn symbols(parser: &mut Parser, text: &str) -> Result<Vec<Symbol>, Error> {
    let tree = parser.parse(text, None).ok_or(Error::Parse)?;
    let source = text.as_bytes();
    let mut found = Vec::new();
    // A stack rather than recursion, so no nesting depth can overflow ours.
    let mut pending = Vec::new();
    push_items(&mut pending, Some(tree.root_node()), "");
    while let Some((item, prefix)) = pending.pop() {
        visit(item, &prefix, source, &mut found, &mut pending);
    }
    number_repeats(&mut found);
    Ok(found)
}

type Pending<'tree> = Vec<(Node<'tree>, String)>;

fn visit<'tree>(
    item: Node<'tree>,
    prefix: &str,
    source: &[u8],
    found: &mut Vec<Symbol>,
    pending: &mut Pending<'tree>,
) {
    if test_only(item, source) {
        return;
    }
    let body = item.child_by_field_name("body");
    if item.kind() == "impl_item" {
        push_items(pending, body, &join(prefix, &impl_owner(item, source)));
        return;
    }
    let name = item
        .child_by_field_name("name")
        .map(|node| plain(text(node, source)));
    let (Some(kind), Some(name)) = (Kind::of(item.kind()), name) else {
        return;
    };
    if kind == Kind::Mod && body.is_none() {
        return;
    }
    let name = join(prefix, name);
    found.push(Symbol {
        kind,
        name: name.clone(),
        line: line(item),
        fingerprint: fingerprint(item, source),
    });
    if matches!(kind, Kind::Mod | Kind::Trait) {
        push_items(pending, body, &name);
    }
}

fn push_items<'tree>(pending: &mut Pending<'tree>, list: Option<Node<'tree>>, prefix: &str) {
    let Some(list) = list else {
        return;
    };
    let mut cursor = list.walk();
    let items = list.named_children(&mut cursor);
    push_reversed(pending, items.map(|item| (item, prefix.to_owned())));
}

/// `Frame` for `impl<T> Frame<T>`, `Frame::Display` for `impl fmt::Display for Frame`.
fn impl_owner(item: Node<'_>, source: &[u8]) -> String {
    let named = |name| {
        item.child_by_field_name(name)
            .map(|node| base_name(node, source))
    };
    let owner = named("type").unwrap_or_default();
    match named("trait") {
        Some(name) => format!("{owner}::{name}"),
        None => owner,
    }
}

/// `Frame` for `fmt::Frame`, `Frame<T>`, `&mut Frame` or `dyn Frame`; `T` for `[T]`.
fn base_name(node: Node<'_>, source: &[u8]) -> String {
    let mut base = node;
    while let Some(inner) = match base.kind() {
        "generic_type" | "reference_type" | "pointer_type" => base.child_by_field_name("type"),
        "scoped_type_identifier" | "scoped_identifier" => base.child_by_field_name("name"),
        "dynamic_type" => base.child_by_field_name("trait"),
        _ => None,
    } {
        base = inner;
    }
    let name = text(base, source);
    let name = name.strip_prefix("r#").unwrap_or(name);
    name.chars()
        .filter(|it| it.is_alphanumeric() || *it == '_')
        .collect()
}

fn test_only(item: Node<'_>, source: &[u8]) -> bool {
    std::iter::successors(item.prev_named_sibling(), Node::prev_named_sibling)
        .take_while(|node| {
            matches!(
                node.kind(),
                "attribute_item" | "line_comment" | "block_comment"
            )
        })
        .any(|node| compact(text(node, source)) == "#[cfg(test)]")
}

/// Repeated names, such as one function per platform, become `name`, `name[2]`, `name[3]`.
fn number_repeats(found: &mut [Symbol]) {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for symbol in found {
        let count = seen.entry(symbol.name.clone()).or_default();
        *count += 1;
        if *count > 1 {
            symbol.name = format!("{}[{count}]", symbol.name);
        }
    }
}

fn fingerprint(item: Node<'_>, source: &[u8]) -> Fingerprint {
    let mut tokens = Vec::new();
    preorder(item, |node| {
        let comment = matches!(node.kind(), "line_comment" | "block_comment");
        if !comment && node.child_count() == 0 {
            tokens.extend(source.get(node.byte_range()));
        }
        !comment
    });
    Fingerprint::of(tokens)
}

/// Visits `root` and what's inside it in source order, descending where `visit` says so,
/// without recursion.
pub(crate) fn preorder<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> bool) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if visit(node) {
            push_reversed(&mut pending, node.children(&mut node.walk()));
        }
    }
}

/// Pushes `items` last first, so they come off the stack in order.
fn push_reversed<T>(stack: &mut Vec<T>, items: impl IntoIterator<Item = T>) {
    let start = stack.len();
    stack.extend(items);
    if let Some(pushed) = stack.get_mut(start..) {
        pushed.reverse();
    }
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or_default()
}

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
    }
}

fn line(node: Node<'_>) -> usize {
    node.start_position().row.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(text: &str) -> Vec<Symbol> {
        symbols(&mut rust_parser().unwrap(), text).unwrap()
    }

    fn names(text: &str) -> Vec<String> {
        outline(text)
            .into_iter()
            .map(|symbol| format!("{} {} {}", symbol.kind, symbol.name, symbol.line))
            .collect()
    }

    fn print(text: &str, name: &str) -> Fingerprint {
        outline(text)
            .into_iter()
            .find(|symbol| symbol.name == name)
            .unwrap()
            .fingerprint
    }

    #[test]
    fn lists_every_kind_of_item_with_its_line() {
        let text = "const A: u8 = 1;\nenum B { X }\nfn c() {}\nmacro_rules! d { () => {} }\n\
                    static E: u8 = 1;\nstruct F;\ntrait G {}\ntype H = u8;\nunion I { x: u8 }\n\
                    mod j {}\nuse k::L;\nextern crate m;\n";
        assert_eq!(
            names(text),
            [
                "const A 1",
                "enum B 2",
                "fn c 3",
                "macro d 4",
                "static E 5",
                "struct F 6",
                "trait G 7",
                "type H 8",
                "union I 9",
                "mod j 10"
            ]
        );
    }

    #[test]
    fn names_nested_items_by_their_owner() {
        let text = "mod a { pub fn b() {} }\ntrait C { fn d(&self); fn e() {} }\n\
                    impl<T> F<T> { fn g() {} }\nimpl fmt::Display for F<u8> { fn fmt() {} }\n\
                    impl crate::x::H { fn i() {} }\n";
        assert_eq!(
            names(text),
            [
                "mod a 1",
                "fn a::b 1",
                "trait C 2",
                "fn C::d 2",
                "fn C::e 2",
                "fn F::g 3",
                "fn F::Display::fmt 4",
                "fn H::i 5"
            ]
        );
    }

    #[test]
    fn leaves_out_test_only_items() {
        let text = "fn a() {}\n#[cfg(test)]\n// helpers\nfn b() {}\n#[cfg( test )]\nmod tests { fn c() {} }\n\
                    #[cfg(unix)]\nfn d() {}\n";
        assert_eq!(names(text), ["fn a 1", "fn d 8"]);
    }

    #[test]
    fn numbers_repeated_names() {
        let text = "#[cfg(unix)]\nfn a() {}\n#[cfg(windows)]\nfn a() {}\nfn a() {}\nfn b() {}\n";
        assert_eq!(names(text), ["fn a 2", "fn a[2] 4", "fn a[3] 5", "fn b 6"]);
    }

    #[test]
    fn fingerprints_ignore_layout_comments_and_attributes() {
        let plain = print("fn a(x: u8) -> u8 { x + 1 }", "a");
        let styled = "/// Adds one.\n#[must_use]\nfn a(\n    x: u8\n) -> u8 {\n    // add\n    x /* one */ + 1\n}\n";
        assert_eq!(print(styled, "a"), plain);
        assert_ne!(print("fn a(x: u8) -> u8 { x + 2 }", "a"), plain);
        assert_ne!(print("fn a(x: u8) -> u8 { x + 1 } fn b() {}", "b"), plain);
    }

    #[test]
    fn outlines_a_half_edited_file() {
        let text = "fn a() {}\nfn b( {\nstruct C;\nfn d() {}\n";
        let found = names(text);
        assert!(found.contains(&"fn a 1".to_owned()), "{found:?}");
        assert!(found.contains(&"fn d 4".to_owned()), "{found:?}");
    }

    #[test]
    fn a_fingerprint_covers_only_its_item() {
        let alone = print("fn a() { 1 }", "a");
        assert_eq!(print("fn a() { 1 }\nfn b() { 2 }", "a"), alone);
        assert_eq!(print("mod m { fn a() { 1 } fn b() {} }", "m::a"), alone);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let depth = 8_000;
        let text = format!("fn a() {{ {}1{} }}", "(".repeat(depth), ")".repeat(depth));
        assert_eq!(names(&text), ["fn a 1"]);
    }

    #[test]
    fn names_raw_identifiers_and_unusual_owners_so_anchors_can_hold_them() {
        let text = "fn r#match() {}\nimpl<T> Tr for &T { fn a() {} }\nimpl Tr for [u8] { fn b() {} }\n\
                    impl dyn r#Tr { fn c() {} }\nimpl<T> Tr for *const T { fn d() {} }\n";
        assert_eq!(
            names(text),
            [
                "fn match 1",
                "fn T::Tr::a 2",
                "fn u8::Tr::b 3",
                "fn Tr::c 4",
                "fn T::Tr::d 5"
            ]
        );
    }

    #[test]
    fn fingerprints_see_string_contents() {
        assert_ne!(
            print(r#"const A: &str = "x";"#, "A"),
            print(r#"const A: &str = "y";"#, "A")
        );
    }
}
