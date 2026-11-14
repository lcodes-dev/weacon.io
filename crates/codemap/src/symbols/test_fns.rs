use tree_sitter::Node;

use super::{Item, Kind, Place, Symbol, fingerprint, join, plain};
use crate::paths::{self, Bound, Segments};
use crate::refs::refs;
use crate::syntax::{attributes, line, text};

/// The `#[test]` functions in `item`, a test-only item or a test, each read where it is
/// written, with the `use`s of the module it is in.
pub(super) fn tests(item: Node<'_>, place: &Place, source: &[u8]) -> Vec<Item> {
    let mut found = Vec::new();
    let mut modules = vec![(item, place.clone(), Vec::new())];
    while let Some((node, place, imports)) = modules.pop() {
        let name = node
            .child_by_field_name("name")
            .map(|name| plain(text(name, source)));
        match (node.kind(), name, node.child_by_field_name("body")) {
            ("mod_item", Some(name), Some(body)) => {
                let inside = Place {
                    within: [&place.within[..], &[name.to_owned()]].concat(),
                    owner: None,
                    prefix: join(&place.prefix, name),
                };
                let mut cursor = body.walk();
                let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
                let own = children
                    .iter()
                    .filter(|child| child.kind() == "use_declaration")
                    .flat_map(|child| paths::imports(*child, source));
                let mut own: Vec<(Segments, Bound)> = own.collect();
                // `use super::*` brings in what the module around it brought in, after its own.
                let glob = own
                    .iter()
                    .any(|(path, bound)| *bound == Bound::Glob && path == &["super"]);
                if glob {
                    own.extend(imports.iter().filter_map(from_child));
                }
                let imports = own;
                modules.extend(
                    children
                        .into_iter()
                        .map(|child| (child, inside.clone(), imports.clone())),
                );
            }
            ("function_item", Some(name), _) if is_test(node, source) => found.push(Item {
                symbol: Symbol {
                    kind: Kind::Fn,
                    name: join(&place.prefix, name),
                    line: line(node),
                    fingerprint: fingerprint(node, source),
                },
                within: place.within.clone(),
                owner: None,
                imports: [imports, paths::local_imports(node, source)].concat(),
                refs: refs(node, source),
                contracts: Vec::new(),
            }),
            _ => {}
        }
    }
    found
}

/// A name its parent module's `use` binds, as a child module reaches it: `self` becomes
/// `super`, and `super` gains another.
fn from_child((path, bound): &(Segments, Bound)) -> Option<(Segments, Bound)> {
    let Bound::Name(_) = bound else {
        return None;
    };
    let up = ["super".to_owned()];
    let path = match path.first().map(String::as_str) {
        Some("self") => [&up[..], path.get(1..).unwrap_or_default()].concat(),
        Some("super") => [&up[..], path].concat(),
        _ => path.clone(),
    };
    Some((path, bound.clone()))
}

/// Whether `function` has `#[test]`, or a test attribute such as `#[tokio::test(...)]`.
pub(super) fn is_test(function: Node<'_>, source: &[u8]) -> bool {
    attributes(function).any(|node| {
        let attribute: String = text(node, source).split_whitespace().collect();
        let inner = attribute.strip_prefix("#[").unwrap_or_default();
        let path = inner.split(['(', ']']).next().unwrap_or_default();
        path == "test" || path.ends_with("::test")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parents_binding_is_rewritten_for_the_child() {
        let rewrite = |path: &[&str]| {
            let path = path.iter().map(|part| (*part).to_owned()).collect();
            let found = from_child(&(path, Bound::Name("x".to_owned())));
            found.map(|(path, _)| path.join("::"))
        };
        assert_eq!(
            rewrite(&["self", "m", "go"]).as_deref(),
            Some("super::m::go")
        );
        assert_eq!(
            rewrite(&["super", "go"]).as_deref(),
            Some("super::super::go")
        );
        assert_eq!(rewrite(&["crate", "go"]).as_deref(), Some("crate::go"));
        assert_eq!(from_child(&(vec!["m".to_owned()], Bound::Glob)), None);
    }
}
