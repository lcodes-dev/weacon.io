use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use super::{Contract, Declared, Declares, declared, literal};
use crate::symbols::base_name;
use crate::syntax::{attributes, text};

/// How deep subcommands nest before the rest are left out, so a loop can't go on.
const DEPTH: usize = 8;

/// A subcommand as its enum declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Variant {
    pub(crate) name: String,
    /// The enum it holds, whose subcommands go under it, or beside it when flattened.
    pub(crate) nests: Option<String>,
    pub(crate) flatten: bool,
}

/// The subcommand enums of each crate, by crate and enum name, with their variants.
pub(crate) type Commands<'a> = BTreeMap<(&'a str, &'a str), Vec<(Contract, &'a Variant)>>;

/// The variants of an enum that derives `Subcommand`, named as clap names them.
pub(crate) fn variants(item: Node<'_>, source: &[u8]) -> Vec<Declared> {
    let Some(list) = item.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    let variants = list
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "enum_variant");
    variants
        .filter_map(|variant| {
            let (renamed, flatten) = settings(variant, source);
            let name = renamed.or_else(|| {
                let name = variant.child_by_field_name("name")?;
                Some(kebab(text(name, source)))
            })?;
            let nests = variant
                .child_by_field_name("body")
                .filter(|body| body.kind() == "ordered_field_declaration_list")
                .and_then(|body| body.child_by_field_name("type"))
                .map(|nested| base_name(nested, source));
            let found = Variant {
                name,
                nests,
                flatten,
            };
            Some(declared(variant, Declares::Command(found)))
        })
        .collect()
}

/// A variant's `#[command(name = "...")]`, and whether it has `#[command(flatten)]`.
fn settings(variant: Node<'_>, source: &[u8]) -> (Option<String>, bool) {
    let mut name = None;
    let mut flatten = false;
    for attribute in attributes(variant).filter_map(|node| node.named_child(0)) {
        let called = attribute.named_child(0).map(|name| text(name, source));
        let Some(arguments) = attribute.child_by_field_name("arguments") else {
            continue;
        };
        if called != Some("command") {
            continue;
        }
        let mut cursor = arguments.walk();
        let tokens: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
        flatten |= tokens.iter().any(|token| text(*token, source) == "flatten");
        let given = tokens.windows(2).find_map(|pair| match pair {
            [key, value] if text(*key, source) == "name" && value.kind() == "string_literal" => {
                Some(literal(*value, source))
            }
            _ => None,
        });
        name = name.or(given);
    }
    (name, flatten)
}

/// `check-anchors` for `CheckAnchors` and `http-serve` for `HTTPServe`, as clap names them.
fn kebab(name: &str) -> String {
    let letters: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (index, letter) in letters.iter().enumerate() {
        let before = index.checked_sub(1).and_then(|at| letters.get(at));
        let after = letters.get(index + 1);
        let starts_word = letter.is_uppercase()
            && before.is_some_and(|before| {
                !before.is_uppercase() || after.is_some_and(|after| after.is_lowercase())
            });
        if starts_word {
            out.push('-');
        }
        out.extend(letter.to_lowercase());
    }
    out
}

/// Each subcommand named after the ones above it, from the enums no other enum nests.
pub(crate) fn full_names(commands: &Commands<'_>) -> Vec<Contract> {
    let nested: BTreeSet<(&str, &str)> = commands
        .iter()
        .flat_map(|((krate, _), variants)| {
            let nests = variants
                .iter()
                .filter_map(|(_, variant)| variant.nests.as_deref());
            nests.map(|nests| (*krate, nests))
        })
        .collect();
    let mut found = Vec::new();
    let mut pending: Vec<((&str, &str), String, usize)> = commands
        .keys()
        .filter(|key| !nested.contains(key))
        .map(|key| (*key, String::new(), 0))
        .collect();
    while let Some(((krate, owner), prefix, depth)) = pending.pop() {
        let Some(variants) = commands.get(&(krate, owner)).filter(|_| depth < DEPTH) else {
            continue;
        };
        for (contract, variant) in variants {
            let name = match (prefix.as_str(), variant.flatten) {
                (_, true) => prefix.clone(),
                ("", false) => variant.name.clone(),
                (prefix, false) => format!("{prefix} {}", variant.name),
            };
            if let Some(nests) = &variant.nests {
                pending.push(((krate, nests), name.clone(), depth + 1));
            }
            if !variant.flatten {
                found.push(Contract {
                    name,
                    ..contract.clone()
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_cases_like_clap() {
        let names = ["CheckAnchors", "Map", "HTTPServe", "ServeHTTP", "V2Api"];
        let found: Vec<String> = names.iter().map(|name| kebab(name)).collect();
        assert_eq!(
            found,
            ["check-anchors", "map", "http-serve", "serve-http", "v2-api"]
        );
    }
}
