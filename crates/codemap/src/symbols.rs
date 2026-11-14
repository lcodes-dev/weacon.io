use std::collections::BTreeMap;
use std::fmt;

use tree_sitter::{Node, Tree};

use crate::Fingerprint;
use crate::contracts::{Declared, contracts};
use crate::paths::{Segments, segments};
use crate::refs::{Ref, refs};
use crate::syntax::{attributes, line, preorder, push_reversed, test_only, text};

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

/// A symbol, where it sits in its file, and what its code refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) symbol: Symbol,
    /// The inline modules around it, outermost first.
    pub(crate) within: Vec<String>,
    /// The type or trait it belongs to, which `Self` names, as written in `within`.
    pub(crate) owner: Option<String>,
    pub(crate) refs: Vec<Ref>,
    pub(crate) contracts: Vec<Declared>,
}

/// A file's items, its trait impls, and its tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outline {
    pub(crate) items: Vec<Item>,
    pub(crate) impls: Vec<Impl>,
    /// Each `#[test]` function, named after the modules around it, such as `tests::reads`.
    pub(crate) tests: Vec<Item>,
}

/// An `impl Trait for Type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Impl {
    pub(crate) within: Vec<String>,
    pub(crate) owner: String,
    /// The trait, as written.
    pub(crate) trait_path: Segments,
}

impl Impl {
    /// The impl as an item of its type that uses the trait, so the trait resolves as any path.
    pub(crate) fn as_item(&self) -> Item {
        let name = [&self.within[..], std::slice::from_ref(&self.owner)]
            .concat()
            .join("::");
        Item {
            symbol: Symbol {
                kind: Kind::Struct,
                name,
                line: 0,
                fingerprint: Fingerprint::of([""]),
            },
            within: self.within.clone(),
            owner: Some(self.owner.clone()),
            refs: vec![Ref::Use(self.trait_path.clone())],
            contracts: Vec::new(),
        }
    }
}

/// Where items sit: the inline modules around them, their owner, and the prefix of their names.
#[derive(Clone, Default)]
struct Place {
    within: Vec<String>,
    owner: Option<String>,
    prefix: String,
}

/// The outline of a file, its items in source order and test-only ones left out.
pub(crate) fn symbols(tree: &Tree, source: &[u8]) -> Outline {
    let mut found = Outline::default();
    // A stack rather than recursion, so no nesting depth can overflow ours.
    let mut pending = Vec::new();
    push_items(&mut pending, Some(tree.root_node()), &Place::default());
    while let Some((item, place)) = pending.pop() {
        visit(item, &place, source, &mut found, &mut pending);
    }
    number_repeats(&mut found.items);
    found
}

type Pending<'tree> = Vec<(Node<'tree>, Place)>;

fn visit<'tree>(
    item: Node<'tree>,
    place: &Place,
    source: &[u8],
    found: &mut Outline,
    pending: &mut Pending<'tree>,
) {
    if test_only(item, source) {
        found.tests.extend(tests(item, place, source));
        return;
    }
    let body = item.child_by_field_name("body");
    if item.kind() == "impl_item" {
        visit_impl(item, place, source, found, pending);
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
    let full = join(&place.prefix, name);
    let symbol = Symbol {
        kind,
        name: full.clone(),
        line: line(item),
        fingerprint: fingerprint(item, source),
    };
    let inside = place.inside(kind, name, full);
    let (refs, contracts) = if inside.is_some() {
        (Vec::new(), Vec::new())
    } else {
        (refs(item, source), contracts(item, source))
    };
    found.items.push(Item {
        symbol,
        within: place.within.clone(),
        owner: place.owner.clone(),
        refs,
        contracts,
    });
    if let Some(inside) = inside {
        push_items(pending, body, &inside);
    }
}

fn visit_impl<'tree>(
    item: Node<'tree>,
    place: &Place,
    source: &[u8],
    found: &mut Outline,
    pending: &mut Pending<'tree>,
) {
    let body = item.child_by_field_name("body");
    let (owner, path) = impl_owner(item, source);
    let written = item.child_by_field_name("trait");
    if let Some(trait_path) = written.and_then(|written| trait_path(written, source)) {
        found.impls.push(Impl {
            within: place.within.clone(),
            owner: owner.clone(),
            trait_path,
        });
    }
    let inside = Place {
        within: place.within.clone(),
        owner: Some(owner),
        prefix: join(&place.prefix, &path),
    };
    push_items(pending, body, &inside);
}

impl Place {
    /// Where the items inside a module or a trait sit; `None` for other kinds.
    fn inside(&self, kind: Kind, name: &str, full: String) -> Option<Self> {
        let within = match kind {
            Kind::Mod => [&self.within[..], &[name.to_owned()]].concat(),
            Kind::Trait => self.within.clone(),
            Kind::Const
            | Kind::Enum
            | Kind::Fn
            | Kind::Macro
            | Kind::Static
            | Kind::Struct
            | Kind::Type
            | Kind::Union => return None,
        };
        Some(Self {
            within,
            owner: (kind == Kind::Trait).then(|| name.to_owned()),
            prefix: full,
        })
    }
}

fn push_items<'tree>(pending: &mut Pending<'tree>, list: Option<Node<'tree>>, place: &Place) {
    let Some(list) = list else {
        return;
    };
    let mut cursor = list.walk();
    let items = list.named_children(&mut cursor);
    push_reversed(pending, items.map(|item| (item, place.clone())));
}

/// The type an `impl` is for, and the prefix of its items' names: `Frame` for
/// `impl<T> Frame<T>`, `Frame::Display` for `impl fmt::Display for Frame`.
fn impl_owner(item: Node<'_>, source: &[u8]) -> (String, String) {
    let base_of = |name| {
        item.child_by_field_name(name)
            .map(|node| base_name(node, source))
    };
    let owner = base_of("type").unwrap_or_default();
    let path = match base_of("trait") {
        Some(name) => format!("{owner}::{name}"),
        None => owner.clone(),
    };
    (owner, path)
}

/// `fmt::Display` for `fmt::Display` or `From<u8>`'s `From`.
fn trait_path(node: Node<'_>, source: &[u8]) -> Option<Segments> {
    match node.kind() {
        "generic_type" => trait_path(node.child_by_field_name("type")?, source),
        "type_identifier" => Some(vec![text(node, source).to_owned()]),
        _ => segments(node, source),
    }
}

/// The `#[test]` functions in `item`, a test-only item, each read as if written in `place`.
fn tests(item: Node<'_>, place: &Place, source: &[u8]) -> Vec<Item> {
    let mut found = Vec::new();
    let mut modules = vec![(item, place.prefix.clone())];
    while let Some((node, prefix)) = modules.pop() {
        let name = node
            .child_by_field_name("name")
            .map(|name| plain(text(name, source)));
        let body = node.child_by_field_name("body");
        match (node.kind(), name) {
            ("mod_item", Some(name)) => {
                let prefix = join(&prefix, name);
                if let Some(body) = body {
                    let mut cursor = body.walk();
                    let children = body.named_children(&mut cursor);
                    modules.extend(children.map(|child| (child, prefix.clone())));
                }
            }
            ("function_item", Some(name)) if is_test(node, source) => found.push(Item {
                symbol: Symbol {
                    kind: Kind::Fn,
                    name: join(&prefix, name),
                    line: line(node),
                    fingerprint: fingerprint(node, source),
                },
                within: place.within.clone(),
                owner: None,
                refs: refs(node, source),
                contracts: Vec::new(),
            }),
            _ => {}
        }
    }
    found
}

fn is_test(function: Node<'_>, source: &[u8]) -> bool {
    attributes(function).any(|node| {
        let attribute: String = text(node, source).split_whitespace().collect();
        attribute == "#[test]" || attribute.ends_with("::test]")
    })
}

/// `Frame` for `fmt::Frame`, `Frame<T>`, `&mut Frame` or `dyn Frame`; `T` for `[T]`.
pub(crate) fn base_name(node: Node<'_>, source: &[u8]) -> String {
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
    let name = plain(name);
    name.chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect()
}

/// Repeated names, such as one function per platform, become `name`, `name[2]`, `name[3]`.
fn number_repeats(found: &mut [Item]) {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for Item { symbol, .. } in found {
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
        let skipped = matches!(
            node.kind(),
            "line_comment" | "block_comment" | "attribute_item" | "inner_attribute_item"
        );
        if !skipped && node.child_count() == 0 {
            tokens.extend(source.get(node.byte_range()));
        }
        !skipped
    });
    Fingerprint::of(tokens)
}

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tree;

    fn symbols_in(text: &str) -> Vec<Symbol> {
        let tree = tree(text);
        symbols(&tree, text.as_bytes())
            .items
            .into_iter()
            .map(|item| item.symbol)
            .collect()
    }

    fn names(text: &str) -> Vec<String> {
        symbols_in(text)
            .into_iter()
            .map(|symbol| format!("{} {} {}", symbol.kind, symbol.name, symbol.line))
            .collect()
    }

    fn fingerprint_of(text: &str, name: &str) -> Fingerprint {
        symbols_in(text)
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
    fn leaves_out_items_whose_cfg_lists_test_in_all() {
        let text = "#[cfg(all(test, unix))]\nfn a() {}\n#[cfg(all(unix, all(windows, test)))]\nfn b() {}\n\
                    #[cfg(any(test, unix))]\nfn c() {}\n#[cfg(not(test))]\nfn d() {}\n\
                    #[cfg(all(unix, any(test, windows)))]\nfn e() {}\n#[cfg(feature = \"test\")]\nfn f() {}\n\
                    #[cfg(all(unix,test))]\nfn g() {}\n";
        assert_eq!(
            names(text),
            ["fn b 4", "fn c 6", "fn d 8", "fn e 10", "fn f 12"]
        );
    }

    #[test]
    fn numbers_repeated_names() {
        let text = "#[cfg(unix)]\nfn a() {}\n#[cfg(windows)]\nfn a() {}\nfn a() {}\nfn b() {}\n";
        assert_eq!(names(text), ["fn a 2", "fn a[2] 4", "fn a[3] 5", "fn b 6"]);
    }

    #[test]
    fn fingerprints_ignore_layout_comments_and_attributes() {
        let plain = fingerprint_of("fn a(x: u8) -> u8 { x + 1 }", "a");
        let styled = "/// Adds one.\n#[must_use]\nfn a(\n    x: u8\n) -> u8 {\n    // add\n    x /* one */ + 1\n}\n";
        assert_eq!(fingerprint_of(styled, "a"), plain);
        assert_ne!(fingerprint_of("fn a(x: u8) -> u8 { x + 2 }", "a"), plain);
        assert_ne!(
            fingerprint_of("fn a(x: u8) -> u8 { x + 1 } fn b() {}", "b"),
            plain
        );
        let module = fingerprint_of("mod m { fn a() {} }", "m");
        assert_eq!(
            fingerprint_of("mod m {\n#![allow(dead_code)]\n#[inline]\nfn a() {} }", "m"),
            module
        );
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
        let alone = fingerprint_of("fn a() { 1 }", "a");
        assert_eq!(fingerprint_of("fn a() { 1 }\nfn b() { 2 }", "a"), alone);
        assert_eq!(
            fingerprint_of("mod m { fn a() { 1 } fn b() {} }", "m::a"),
            alone
        );
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
            fingerprint_of(r#"const A: &str = "x";"#, "A"),
            fingerprint_of(r#"const A: &str = "y";"#, "A")
        );
    }
}
