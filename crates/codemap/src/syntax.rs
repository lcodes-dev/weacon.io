use tree_sitter::{Node, Parser, Tree};

use crate::Error;
use crate::cfg::implies_test;

pub(crate) fn rust_parser() -> Result<Parser, Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|_| Error::Grammar)?;
    Ok(parser)
}

pub(crate) fn parse(parser: &mut Parser, text: &str) -> Result<Tree, Error> {
    parser.parse(text, None).ok_or(Error::Parse)
}

/// Visits `root` and what's inside it in source order, descending where `visit` says so,
/// without recursion, so no nesting depth can overflow the stack.
pub(crate) fn preorder<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> bool) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if visit(node) {
            push_reversed(&mut pending, node.children(&mut node.walk()));
        }
    }
}

/// Pushes `items` last first, so they come off the stack in order.
pub(crate) fn push_reversed<T>(stack: &mut Vec<T>, items: impl IntoIterator<Item = T>) {
    let start = stack.len();
    stack.extend(items);
    if let Some(pushed) = stack.get_mut(start..) {
        pushed.reverse();
    }
}

pub(crate) fn test_only(item: Node<'_>, source: &[u8]) -> bool {
    std::iter::successors(item.prev_named_sibling(), Node::prev_named_sibling)
        .take_while(|node| {
            matches!(
                node.kind(),
                "attribute_item" | "line_comment" | "block_comment"
            )
        })
        .any(|node| {
            let attribute = compact(text(node, source));
            let predicate = attribute
                .strip_prefix("#[cfg(")
                .and_then(|it| it.strip_suffix(")]"));
            predicate.is_some_and(implies_test)
        })
}

pub(crate) fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or_default()
}

fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_in_source_order_and_skips_what_it_is_told_to() {
        let code = "fn a() { 1 } fn b() { 2 }";
        let tree = parse(&mut rust_parser().unwrap(), code).unwrap();
        let mut seen = Vec::new();
        preorder(tree.root_node(), |node| {
            let node = text(node, code.as_bytes());
            seen.push(node);
            node != "fn a() { 1 }"
        });
        let leaves: Vec<&str> = seen.iter().copied().filter(|it| it.len() == 1).collect();
        assert_eq!(leaves, ["b", "(", ")", "{", "2", "}"]);
        assert_eq!(seen[1..3], ["fn a() { 1 }", "fn b() { 2 }"]);
    }

    #[test]
    fn pushes_items_to_pop_in_order() {
        let mut stack = vec![0];
        push_reversed(&mut stack, [1, 2, 3]);
        assert_eq!(stack, [0, 3, 2, 1]);
    }
}
