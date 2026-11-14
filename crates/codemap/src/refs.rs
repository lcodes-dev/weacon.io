use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use tree_sitter::Node;

use crate::paths::{Segments, segments};
use crate::syntax::{preorder, text};

/// A name an item's code refers to, as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Ref {
    /// `f()`, `a::f()` or `m!()`.
    Call(Segments),
    /// `x.f()`: only the method's name is known.
    Method(String),
    /// A type or a path that isn't called: `Frame`, `a::B`, `C::D`.
    Use(Segments),
}

/// What `item` refers to, each once, in order. A trait's body is left out: its items are
/// read on their own.
pub(crate) fn refs(item: Node<'_>, source: &[u8]) -> Vec<Ref> {
    let generics = generics(item, source);
    let body = Some(item)
        .filter(|item| item.kind() == "trait_item")
        .and_then(|item| item.child_by_field_name("body"))
        .map(|body| body.id());
    let mut found = BTreeSet::new();
    let mut called = BTreeSet::new();
    preorder(item, |node| {
        if Some(node.id()) == body {
            return false;
        }
        match node.kind() {
            "call_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    found.extend(call(function, source, &mut called));
                }
                true
            }
            "macro_invocation" => {
                let name = node.child_by_field_name("macro");
                found.extend(name.and_then(|name| segments(name, source)).map(Ref::Call));
                false
            }
            // What a `use` names is read when a name it binds is used.
            "use_declaration" => false,
            "type_identifier" => {
                found.insert(Ref::Use(vec![text(node, source).to_owned()]));
                false
            }
            "identifier" if is_constant(text(node, source)) && !called.contains(&node.id()) => {
                found.insert(Ref::Use(vec![text(node, source).to_owned()]));
                false
            }
            "scoped_identifier" | "scoped_type_identifier" => match segments(node, source) {
                Some(path) => {
                    if !called.contains(&node.id()) {
                        found.insert(Ref::Use(path));
                    }
                    // Type arguments, as in `Store::<A>::new`, are read on their own.
                    text(node, source).contains('<')
                }
                None => true,
            },
            _ => true,
        }
    });
    found
        .into_iter()
        .filter(|found| !rooted_in(found, &generics))
        .collect()
}

/// `LIMIT` or `MAX_2`: how a constant or a static is named.
fn is_constant(name: &str) -> bool {
    name.chars().any(|ch| ch.is_ascii_uppercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

/// Whether a path starts at one of the item's type parameters, as `T::make` does.
fn rooted_in(found: &Ref, generics: &BTreeSet<&str>) -> bool {
    match found {
        Ref::Call(path) | Ref::Use(path) => path
            .first()
            .is_some_and(|first| generics.contains(first.as_str())),
        Ref::Method(_) => false,
    }
}

/// The names of `item`'s type parameters.
fn generics<'a>(item: Node<'_>, source: &'a [u8]) -> BTreeSet<&'a str> {
    let Some(list) = item.child_by_field_name("type_parameters") else {
        return BTreeSet::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter_map(|parameter| parameter.child_by_field_name("name"))
        .map(|name| text(name, source))
        .collect()
}

/// The call `function` makes, marking the nodes it names so they aren't also read as uses.
fn call(function: Node<'_>, source: &[u8], called: &mut BTreeSet<usize>) -> Option<Ref> {
    let function = match function.kind() {
        "generic_function" => function.child_by_field_name("function")?,
        _ => function,
    };
    match function.kind() {
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            Some(Ref::Method(text(field, source).to_owned()))
        }
        "identifier" | "scoped_identifier" => {
            let path = segments(function, source)?;
            called.insert(function.id());
            Some(Ref::Call(path))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tree;

    fn found(text: &str) -> Vec<String> {
        let tree = tree(text);
        let item = tree.root_node().named_child(0).unwrap();
        refs(item, text.as_bytes())
            .into_iter()
            .map(|found| match found {
                Ref::Call(path) => format!("call {}", path.join("::")),
                Ref::Method(name) => format!("method {name}"),
                Ref::Use(path) => format!("use {}", path.join("::")),
            })
            .collect()
    }

    #[test]
    fn tells_calls_from_uses() {
        let text = "fn f(x: Frame) -> a::B { g(); h::i(); j::<u8>(); x.k(); l!(); m::n!(); \
                    let o = P { q: r::S::new() }; Self::t(); u::V }";
        assert_eq!(
            found(text),
            [
                "call Self::t",
                "call g",
                "call h::i",
                "call j",
                "call l",
                "call m::n",
                "call r::S::new",
                "method k",
                "use Frame",
                "use P",
                "use a::B",
                "use u::V",
            ]
        );
    }

    #[test]
    fn calls_through_type_arguments_and_uses_them_too() {
        assert_eq!(
            found("fn f() { Store::<A>::new(); }"),
            ["call Store::new", "use A", "use Store"]
        );
    }

    #[test]
    fn reads_constants_but_not_what_starts_at_a_type_parameter() {
        assert_eq!(
            found("fn f<T: Make>(x: u8) { T::make(); T::Assoc; LIMIT; MAX_2; x; Mixed; }"),
            ["use LIMIT", "use MAX_2", "use Make"]
        );
    }

    #[test]
    fn reads_a_trait_header_but_not_its_body() {
        assert_eq!(
            found("trait Derived: Base where Self: Other { fn go(&self) { helper(); } }"),
            ["use Base", "use Derived", "use Other", "use Self"]
        );
    }

    #[test]
    fn leaves_out_type_parameters() {
        assert_eq!(
            found("fn f<T: Bound, U, const N: usize>(_: T, _: U, _: V) {}"),
            ["use Bound", "use V"]
        );
    }

    #[test]
    fn notes_each_name_once() {
        assert_eq!(
            found("fn f() { g(); g(); a::B; a::B; }"),
            ["call g", "use a::B"]
        );
    }
}
