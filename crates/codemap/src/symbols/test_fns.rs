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

/// A name its parent module's `use` binds, as a child module reaches it: relative paths
/// gain a `super`.
fn from_child((path, bound): &(Segments, Bound)) -> Option<(Segments, Bound)> {
    let Bound::Name(_) = bound else {
        return None;
    };
    let relative = path
        .first()
        .is_some_and(|first| first == "self" || first == "super");
    let path = if relative {
        [&["super".to_owned()][..], path].concat()
    } else {
        path.clone()
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
