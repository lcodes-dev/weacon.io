//! The MCP tools code declares: by name to `Tool::new`, or with a `#[tool]` attribute.

use tree_sitter::Node;

use super::{Declared, Declares, declared, literal};
use crate::paths::segments;
use crate::syntax::{attributes, name, text};

/// The tool a `Tool::new("name", …)` call declares; none for a name that isn't a literal.
pub(super) fn created(call: Node<'_>, source: &[u8]) -> Option<Declared> {
    let function = segments(call.child_by_field_name("function")?, source)?;
    let [.., kind, new] = function.as_slice() else {
        return None;
    };
    if kind != "Tool" || new != "new" {
        return None;
    }
    let tool = call
        .child_by_field_name("arguments")?
        .named_child(0)
        .filter(|tool| tool.kind() == "string_literal")?;
    Some(declared(call, Declares::McpTool(literal(tool, source))))
}

/// The tool a `#[tool]` on the function `item` declares: the `name` it gives, else the
/// function's own.
pub(super) fn attributed(item: Node<'_>, source: &[u8]) -> Option<Declared> {
    if item.kind() != "function_item" {
        return None;
    }
    let own = name(item.child_by_field_name("name")?, source);
    attributes(item).find_map(|node| {
        let attribute = node.named_child(0)?;
        let path = segments(attribute.named_child(0)?, source)?;
        if path.last()? != "tool" {
            return None;
        }
        let given = attribute
            .child_by_field_name("arguments")
            .and_then(|arguments| given_name(arguments, source));
        let tool = given.unwrap_or_else(|| own.to_owned());
        Some(declared(node, Declares::McpTool(tool)))
    })
}

/// The string after `name =` among an attribute's arguments, nested ones left out.
fn given_name(arguments: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = arguments.walk();
    let tokens: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
    tokens.windows(2).find_map(|pair| match pair {
        [key, value]
            if key.kind() == "identifier"
                && text(*key, source) == "name"
                && value.kind() == "string_literal" =>
        {
            Some(literal(*value, source))
        }
        _ => None,
    })
}
