use std::collections::BTreeMap;

use crate::edges::id;
use crate::graph::{Links, ModuleNode};
use crate::{Edge, EdgeKind, Symbol};

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 100;

/// Page rank over weighted links; the ranks sum to 1, and a node without links shares its rank
/// with every node. A fixed number of rounds keeps the result the same on every run.
pub(crate) fn page_rank(links: &Links) -> Vec<f64> {
    let count = as_float(links.len());
    let mut ranks = vec![1.0 / count; links.len()];
    for _ in 0..ITERATIONS {
        ranks = next_ranks(links, &ranks, count);
    }
    ranks
}

fn next_ranks(links: &Links, ranks: &[f64], count: f64) -> Vec<f64> {
    let mut next = vec![0.0; ranks.len()];
    let mut shared = 1.0 - DAMPING;
    for (targets, rank) in links.iter().zip(ranks) {
        let total: f64 = targets.values().sum();
        if total <= 0.0 {
            shared += DAMPING * rank;
            continue;
        }
        for (target, weight) in targets {
            if let Some(slot) = next.get_mut(*target) {
                *slot += DAMPING * rank * weight / total;
            }
        }
    }
    next.iter().map(|rank| rank + shared / count).collect()
}

/// Ranks this close count as a tie, so rounding can't decide what leads.
const SAME_RANK: f64 = 1e-9;

/// The names, highest rank first, ties in name order.
pub(crate) fn by_rank(mut ranked: Vec<(&str, f64)>) -> Vec<&str> {
    let rounded = |rank: f64| (rank / SAME_RANK).round();
    ranked.sort_by(|(name, rank), (other_name, other_rank)| {
        rounded(*other_rank)
            .total_cmp(&rounded(*rank))
            .then_with(|| name.cmp(other_name))
    });
    ranked.into_iter().map(|(name, _)| name).collect()
}

/// The nodes' items, most central first: an item gains from those that call, use or implement it.
pub(crate) fn central(nodes: &[ModuleNode<'_>], edges: &[Edge]) -> Vec<String> {
    let items: Vec<String> = nodes
        .iter()
        .flat_map(|&ModuleNode { krate, module }| {
            let path = |symbol: &Symbol| id(&krate.name, &module.path, &symbol.name);
            module.symbols.iter().map(path)
        })
        .collect();
    let numbers: BTreeMap<&str, usize> = (0..)
        .zip(&items)
        .map(|(number, id)| (id.as_str(), number))
        .collect();
    let mut links: Links = vec![BTreeMap::new(); items.len()];
    for edge in edges.iter().filter(|edge| edge.kind != EdgeKind::Tests) {
        if let (Some(from), Some(to)) = (
            numbers.get(edge.from.as_str()),
            numbers.get(edge.to.as_str()),
        ) && let Some(targets) = links.get_mut(*from)
        {
            *targets.entry(*to).or_insert(0.0) += 1.0;
        }
    }
    let ranks = page_rank(&links);
    let ranked = items.iter().map(String::as_str).zip(ranks).collect();
    by_rank(ranked).into_iter().map(str::to_owned).collect()
}

/// `count` as a float, saturating far beyond any repository's size.
fn as_float(count: usize) -> f64 {
    u32::try_from(count).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn links(edges: &[(usize, usize, f64)], nodes: usize) -> Links {
        test_support::links(edges.iter().copied(), nodes)
    }

    fn close(one: f64, other: f64) -> bool {
        (one - other).abs() < 1e-9
    }

    #[test]
    fn ranks_sum_to_one() {
        let ranks = page_rank(&links(
            &[(0, 1, 1.0), (1, 2, 1.0), (2, 0, 1.0), (3, 0, 2.0)],
            5,
        ));
        assert!(close(ranks.iter().sum(), 1.0), "{ranks:?}");
    }

    #[test]
    fn a_cycle_ranks_every_node_alike() {
        let ranks = page_rank(&links(&[(0, 1, 1.0), (1, 2, 1.0), (2, 0, 1.0)], 3));
        assert!(
            ranks.iter().all(|rank| close(*rank, 1.0 / 3.0)),
            "{ranks:?}"
        );
    }

    #[test]
    fn the_node_everyone_points_at_ranks_highest() {
        let ranks = page_rank(&links(
            &[(1, 0, 1.0), (2, 0, 1.0), (3, 0, 1.0), (0, 1, 1.0)],
            4,
        ));
        assert!(ranks[0] > ranks[1] && ranks[1] > ranks[2], "{ranks:?}");
        assert!(close(ranks[2], ranks[3]), "{ranks:?}");
    }

    #[test]
    fn weights_steer_the_rank() {
        let ranks = page_rank(&links(
            &[(0, 1, 3.0), (0, 2, 1.0), (1, 0, 1.0), (2, 0, 1.0)],
            3,
        ));
        assert!(ranks[1] > ranks[2], "{ranks:?}");
    }

    #[test]
    fn known_values_for_a_small_graph() {
        let ranks = page_rank(&links(&[(0, 1, 1.0)], 2));
        // r0 = 0.15 / 2 + 0.85 * r1 / 2, as node 1 shares its rank; r0 + r1 = 1.
        assert!(close(ranks[0], 0.5 / 1.425), "{ranks:?}");
    }

    #[test]
    fn an_empty_graph_has_no_ranks() {
        assert!(page_rank(&Vec::new()).is_empty());
    }

    #[test]
    fn counts_as_floats() {
        assert!(close(as_float(3), 3.0));
        assert!(close(as_float(usize::MAX), f64::from(u32::MAX)));
    }

    #[test]
    fn ranks_the_items_others_rely_on_first() {
        let files = test_support::files(&[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub fn core() {}\npub fn b() { core(); }\npub fn c() { core(); b(); }\n\
                 pub fn lone() {}\n#[cfg(test)]\nmod tests { #[test]\n fn t() { super::lone(); } }\n",
            ),
        ]);
        let index = crate::index(&files).unwrap();
        assert_eq!(index.central, ["a::core", "a::b", "a::c", "a::lone"]);
    }
}
