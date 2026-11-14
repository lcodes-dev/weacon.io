use tree_sitter::Node;

use crate::syntax::{preorder, push_reversed, test_only, text};

/// A path as written in code, split on `::`: `["crate", "frame", "encode_line"]`.
pub(crate) type Segments = Vec<String>;

/// A path, and the inline modules around it within its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedPath {
    pub(crate) within: Segments,
    pub(crate) segments: Segments,
    /// What a `use` brings into scope under this path; `None` for other paths.
    pub(crate) bound: Option<Bound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bound {
    /// The name it is known by: the path's last segment, or its `as` alias.
    Name(String),
    /// Everything the path names, from `use path::*`.
    Glob,
}

type Found = Vec<(Segments, Option<Bound>)>;

type Scopes<'tree> = Vec<(Node<'tree>, Vec<String>)>;

/// Every path a file names outside its test-only items: in `use` trees, in qualified names
/// such as `frame::Frame::new`, and in `mod frame;` declarations.
pub(crate) fn named_paths(root: Node<'_>, source: &[u8]) -> Vec<NamedPath> {
    let mut found = Vec::new();
    let mut scopes = vec![(root, Vec::new())];
    while let Some((scope, within)) = scopes.pop() {
        let mut written = Vec::new();
        preorder(scope, |node| {
            visit(node, source, &within, &mut written, &mut scopes)
        });
        found.extend(written.into_iter().map(|(segments, bound)| NamedPath {
            within: within.clone(),
            segments,
            bound,
        }));
    }
    found
}

/// Collects the paths `node` names, and says whether to look inside it.
fn visit<'tree>(
    node: Node<'tree>,
    source: &[u8],
    within: &[String],
    found: &mut Found,
    scopes: &mut Scopes<'tree>,
) -> bool {
    // Only items take attributes, and checking others is slow in deep trees.
    let item = node.kind().ends_with("_item") || node.kind() == "use_declaration";
    if item && test_only(node, source) {
        return false;
    }
    match node.kind() {
        "use_declaration" => {
            if let Some(tree) = node.child_by_field_name("argument") {
                use_tree(tree, source, found);
            }
            false
        }
        "mod_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|name| text(name, source).to_owned());
            match (node.child_by_field_name("body"), name) {
                (None, Some(name)) => found.push((vec![name], None)),
                (Some(body), Some(name)) => scopes.push((body, [within, &[name]].concat())),
                _ => {}
            }
            false
        }
        "scoped_identifier" | "scoped_type_identifier" => match segments(node, source) {
            Some(path) => {
                found.push((path, None));
                false
            }
            None => true,
        },
        _ => true,
    }
}

fn use_tree(root: Node<'_>, source: &[u8], found: &mut Found) {
    let mut pending = vec![(root, Vec::new())];
    while let Some((node, prefix)) = pending.pop() {
        let path = |node: Option<Node<'_>>| {
            let tail = node.map_or(Some(Vec::new()), |node| segments(node, source))?;
            Some([prefix.as_slice(), &tail].concat())
        };
        match node.kind() {
            "scoped_use_list" => {
                if let Some(list) = node.child_by_field_name("list")
                    && let Some(prefix) = path(node.child_by_field_name("path"))
                {
                    pending.push((list, prefix));
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                let items = node.named_children(&mut cursor);
                push_reversed(&mut pending, items.map(|item| (item, prefix.clone())));
            }
            "use_as_clause" => {
                let alias = node.child_by_field_name("alias");
                let alias = alias.map(|alias| Bound::Name(text(alias, source).to_owned()));
                found.extend(path(node.child_by_field_name("path")).map(|path| (path, alias)));
            }
            "use_wildcard" => {
                found.extend(path(node.named_child(0)).map(|path| (path, Some(Bound::Glob))));
            }
            "self" => found.extend(path(None).map(named_by_last)),
            _ => found.extend(path(Some(node)).map(named_by_last)),
        }
    }
}

fn named_by_last(segments: Segments) -> (Segments, Option<Bound>) {
    let bound = segments.last().map(|last| Bound::Name(last.clone()));
    (segments, bound)
}

/// `a::b::C` as `["a", "b", "C"]`; `None` for paths through types such as `<T as U>::f`.
pub(crate) fn segments(node: Node<'_>, source: &[u8]) -> Option<Segments> {
    let mut reversed = Vec::new();
    let mut next = Some(node);
    while let Some(node) = next {
        let name = match node.kind() {
            "identifier" | "type_identifier" | "crate" | "self" | "super" => {
                next = None;
                node
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                next = node.child_by_field_name("path");
                node.child_by_field_name("name")?
            }
            _ => return None,
        };
        reversed.push(text(name, source).to_owned());
    }
    reversed.reverse();
    Some(reversed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tree;

    fn found(text: &str) -> Vec<String> {
        let tree = tree(text);
        named_paths(tree.root_node(), text.as_bytes())
            .into_iter()
            .map(|path| {
                let mut parts = path.within;
                parts.push(path.segments.join("::"));
                parts.join("/")
            })
            .collect()
    }

    #[test]
    fn notes_the_inline_modules_around_each_path() {
        let text = "use a::B;\nmod m { use super::c; mod n { fn f() { self::g(); } } }\nfn h() { d::E; }\n";
        assert_eq!(found(text), ["a::B", "d::E", "m/super::c", "m/n/self::g"]);
    }

    #[test]
    fn expands_use_trees() {
        let text =
            "use crate::a::{self, b::{C, d as e}, f::*};\nuse g;\nuse ::h::I;\nuse super::j::*;\n";
        assert_eq!(
            found(text),
            [
                "crate::a",
                "crate::a::b::C",
                "crate::a::b::d",
                "crate::a::f",
                "g",
                "h::I",
                "super::j"
            ]
        );
    }

    #[test]
    fn notes_what_each_use_brings_into_scope() {
        let text = "use a::{self, b::C, d as e, f::*};\nfn g() { h::I; }\n";
        let tree = tree(text);
        let bound: Vec<Option<Bound>> = named_paths(tree.root_node(), text.as_bytes())
            .into_iter()
            .map(|path| path.bound)
            .collect();
        let name = |name: &str| Some(Bound::Name(name.to_owned()));
        assert_eq!(
            bound,
            [name("a"), name("C"), name("e"), Some(Bound::Glob), None]
        );
    }

    #[test]
    fn finds_qualified_names_and_module_declarations() {
        let text = "mod a;\nmod b { }\nfn c(x: d::E) -> f::G { h::i(); crate::j!(); <K as l::M>::n(); x }\n";
        assert_eq!(
            found(text),
            ["a", "d::E", "f::G", "h::i", "crate::j", "l::M"]
        );
    }

    #[test]
    fn deep_paths_do_not_overflow_the_stack() {
        // The `use` path is the deep one; a long qualified path takes the same walk.
        let (deep, long) = (20_000, 2_000);
        let text = format!(
            "use {}x;\nfn f() {{ {}y(); }}\n",
            "a::".repeat(deep),
            "b::".repeat(long)
        );
        let lengths: Vec<usize> = found(&text)
            .iter()
            .map(|path| path.split("::").count())
            .collect();
        assert_eq!(lengths, [deep + 1, long + 1]);
    }

    #[test]
    fn leaves_out_test_only_items() {
        let text = "use a::B;\n#[cfg(test)]\nmod tests { use c::D; fn e() { f::g(); } }\n#[cfg(test)]\nuse h::I;\n";
        assert_eq!(found(text), ["a::B"]);
    }
}
