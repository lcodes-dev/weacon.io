use std::collections::BTreeSet;

use tree_sitter::Node;

use crate::paths::{Segments, segments};
use crate::syntax::{preorder, text};

/// A name an item's code refers to, as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Ref {
    /// `f()`, `a::f()` or `m!()`.
    Call(Segments),
    /// `x.f()`: only the method's name is known.
    Method(String),
    /// A type or a path that isn't called: `Frame`, `a::B`, `C::D`.
    Use(Segments),
}

/// What `item` refers to, each once, in order.
pub(crate) fn refs(item: Node<'_>, source: &[u8]) -> Vec<Ref> {
    let generics = generics(item, source);
    let mut found = BTreeSet::new();
    let mut called = BTreeSet::new();
    preorder(item, |node| {
        if called.contains(&node.id()) {
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
                found.extend(name.and_then(|it| segments(it, source)).map(Ref::Call));
                false
            }
            "type_identifier" => {
                let name = text(node, source);
                if !generics.contains(name) {
                    found.insert(Ref::Use(vec![name.to_owned()]));
                }
                false
            }
            "scoped_identifier" | "scoped_type_identifier" => match segments(node, source) {
                Some(path) => {
                    found.insert(Ref::Use(path));
                    false
                }
                None => true,
            },
            _ => true,
        }
    });
    found.into_iter().collect()
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
    use crate::syntax::{parse, rust_parser};

    fn found(text: &str) -> Vec<String> {
        let tree = parse(&mut rust_parser().unwrap(), text).unwrap();
        let item = tree.root_node().named_child(0).unwrap();
        refs(item, text.as_bytes())
            .into_iter()
            .map(|it| match it {
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
    fn reads_a_path_through_a_generic_type_by_its_parts() {
        assert_eq!(found("fn f() { Vec::<A>::new(); }"), ["use A", "use Vec"]);
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
