use tree_sitter::Node;

use super::{Item, Kind, Place, Symbol, fingerprint, join, plain};
use crate::paths;
use crate::refs::refs;
use crate::syntax::{attributes, line, text};

/// The `#[test]` functions in `item`, a test-only item, each read as if written in `place`.
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
                let mut imports = imports.clone();
                for child in children
                    .iter()
                    .filter(|child| child.kind() == "use_declaration")
                {
                    imports.extend(paths::imports(*child, source));
                }
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

fn is_test(function: Node<'_>, source: &[u8]) -> bool {
    attributes(function).any(|node| {
        let attribute: String = text(node, source).split_whitespace().collect();
        attribute == "#[test]" || attribute.ends_with("::test]")
    })
}
