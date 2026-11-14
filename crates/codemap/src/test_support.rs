//! Helpers the crate's tests share.

use std::collections::BTreeMap;

use tree_sitter::Tree;

use crate::SourceFile;
use crate::graph::Links;
use crate::syntax::{parse, rust_parser};

pub(crate) fn file(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.to_owned(),
        text: text.to_owned(),
    }
}

pub(crate) fn files(list: &[(&str, &str)]) -> Vec<SourceFile> {
    list.iter().map(|(path, text)| file(path, text)).collect()
}

pub(crate) fn tree(text: &str) -> Tree {
    parse(&mut rust_parser().unwrap(), text).unwrap()
}

/// Links among `nodes` nodes, from `(from, to, weight)` triples.
pub(crate) fn links(edges: impl IntoIterator<Item = (usize, usize, f64)>, nodes: usize) -> Links {
    let mut links = vec![BTreeMap::new(); nodes];
    for (from, to, weight) in edges {
        links[from].insert(to, weight);
    }
    links
}
