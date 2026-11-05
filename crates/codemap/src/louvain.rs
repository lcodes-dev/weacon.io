use std::collections::BTreeMap;

use crate::graph::Links;

/// How much better a community must be before a node moves, so rounding never moves it back.
const BETTER: f64 = 1e-12;
const MAX_LEVELS: usize = 100;
const MAX_PASSES: usize = 100;

/// Louvain communities of the links taken as undirected, numbered in order of their first node.
///
/// Nodes are visited in order and ties go to a node's own community, then to the lowest one,
/// so the same links always give the same communities.
pub(crate) fn communities(links: &Links) -> Vec<usize> {
    let mut graph = undirected(links);
    let mut membership: Vec<usize> = (0..links.len()).collect();
    for _ in 0..MAX_LEVELS {
        let Some(local) = one_level(&graph) else {
            break;
        };
        membership = membership.iter().map(|it| at(&local, *it)).collect();
        graph = aggregate(&graph, &local);
    }
    renumber(&membership)
}

fn undirected(links: &Links) -> Links {
    let mut graph = vec![BTreeMap::new(); links.len()];
    for (from, targets) in links.iter().enumerate() {
        for (&to, &weight) in targets {
            add(&mut graph, from, to, weight);
            add(&mut graph, to, from, weight);
        }
    }
    graph
}

/// Moves nodes between communities while that raises modularity; `None` when none moved.
fn one_level(graph: &Links) -> Option<Vec<usize>> {
    let degrees: Vec<f64> = graph.iter().map(|targets| targets.values().sum()).collect();
    let total: f64 = degrees.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let start: Vec<usize> = (0..graph.len()).collect();
    let mut community = start.clone();
    let mut totals = degrees.clone();
    for _ in 0..MAX_PASSES {
        let before = community.clone();
        for (node, (targets, &degree)) in graph.iter().zip(&degrees).enumerate() {
            let current = at(&community, node);
            add_to(&mut totals, current, -degree);
            let gain = |to: usize, inside: f64| {
                inside - totals.get(to).copied().unwrap_or(0.0) * degree / total
            };
            let best = best_community(current, &community_weights(targets, &community, node), gain);
            add_to(&mut totals, best, degree);
            if let Some(slot) = community.get_mut(node) {
                *slot = best;
            }
        }
        if community == before {
            break;
        }
    }
    (community != start).then(|| renumber(&community))
}

fn add_to(totals: &mut [f64], community: usize, degree: f64) {
    if let Some(slot) = totals.get_mut(community) {
        *slot += degree;
    }
}

/// The community with the most gain, the current one unless another is clearly better.
fn best_community(
    current: usize,
    weights: &BTreeMap<usize, f64>,
    gain: impl Fn(usize, f64) -> f64,
) -> usize {
    let mut best = (
        current,
        gain(current, weights.get(&current).copied().unwrap_or(0.0)),
    );
    for (&to, &inside) in weights {
        let candidate = gain(to, inside);
        if candidate > best.1 + BETTER {
            best = (to, candidate);
        }
    }
    best.0
}

/// The weight from `node` into each community of its neighbours, its own loop left out.
fn community_weights(
    targets: &BTreeMap<usize, f64>,
    community: &[usize],
    node: usize,
) -> BTreeMap<usize, f64> {
    let mut weights = BTreeMap::new();
    for (&target, &weight) in targets.iter().filter(|(target, _)| **target != node) {
        *weights.entry(at(community, target)).or_insert(0.0) += weight;
    }
    weights
}

/// One node per community, with the links between communities summed.
fn aggregate(graph: &Links, community: &[usize]) -> Links {
    let size = community.iter().max().map_or(0, |last| last + 1);
    let mut next = vec![BTreeMap::new(); size];
    for (from, targets) in graph.iter().enumerate() {
        for (&to, &weight) in targets {
            add(&mut next, at(community, from), at(community, to), weight);
        }
    }
    next
}

fn renumber(membership: &[usize]) -> Vec<usize> {
    let mut numbers = BTreeMap::new();
    membership
        .iter()
        .map(|it| {
            let next = numbers.len();
            *numbers.entry(*it).or_insert(next)
        })
        .collect()
}

fn add(graph: &mut Links, from: usize, to: usize, weight: f64) {
    if let Some(targets) = graph.get_mut(from) {
        *targets.entry(to).or_insert(0.0) += weight;
    }
}

fn at(values: &[usize], index: usize) -> usize {
    values.get(index).copied().unwrap_or(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(edges: &[(usize, usize)], nodes: usize) -> Links {
        let mut links = vec![BTreeMap::new(); nodes];
        for &(from, to) in edges {
            links[from].insert(to, 1.0);
        }
        links
    }

    /// Two triangles, 0-1-2 and 3-4-5, and a bridge from 2 to 3.
    const BRIDGED: &[(usize, usize)] = &[(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)];

    #[test]
    fn splits_two_triangles_at_their_bridge() {
        assert_eq!(communities(&links(BRIDGED, 6)), [0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn link_direction_does_not_matter() {
        let reversed: Vec<(usize, usize)> = BRIDGED.iter().map(|&(from, to)| (to, from)).collect();
        assert_eq!(communities(&links(&reversed, 6)), [0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn merges_communities_over_levels() {
        // Sixteen triangles in a ring: only the second level can merge whole triangles.
        let triangles = 16;
        let mut edges = Vec::new();
        for group in 0..triangles {
            let base = group * 3;
            edges.extend([(base, base + 1), (base + 1, base + 2), (base + 2, base)]);
            edges.push((base + 2, (base + 3) % (triangles * 3)));
        }
        let found = communities(&links(&edges, triangles * 3));
        let distinct: std::collections::BTreeSet<_> = found.iter().collect();
        assert!(distinct.len() < triangles, "{found:?}");
        for group in found.chunks(3) {
            assert!(group.iter().all(|it| *it == group[0]), "{found:?}");
        }
    }

    #[test]
    fn nodes_without_links_stay_alone() {
        assert_eq!(communities(&links(&[(0, 1)], 4)), [0, 0, 1, 2]);
        assert_eq!(communities(&Vec::new()), Vec::<usize>::new());
    }

    #[test]
    fn heavier_links_decide_the_split() {
        let ring = |heavy: [(usize, usize); 2]| {
            let mut graph = links(&[(0, 1), (1, 2), (2, 3), (3, 0)], 4);
            for (from, to) in heavy {
                graph[from].insert(to, 5.0);
            }
            communities(&graph)
        };
        assert_eq!(ring([(0, 1), (2, 3)]), [0, 0, 1, 1]);
        assert_eq!(ring([(1, 2), (3, 0)]), [0, 1, 1, 0]);
    }

    fn weighted(edges: &[(usize, usize)], nodes: usize) -> Vec<usize> {
        communities(&links(edges, nodes))
    }

    #[test]
    fn moves_nodes_again_until_nothing_moves() {
        let edges = [
            (4, 5),
            (1, 3),
            (1, 5),
            (2, 4),
            (0, 4),
            (0, 5),
            (2, 3),
            (1, 4),
            (3, 5),
        ];
        assert_eq!(weighted(&edges, 6), [0, 0, 1, 1, 0, 0]);
    }

    #[test]
    fn keeps_each_community_total_in_step() {
        let edges = [
            (3, 6),
            (1, 3),
            (2, 3),
            (4, 6),
            (5, 6),
            (2, 6),
            (0, 5),
            (0, 1),
            (1, 2),
        ];
        assert_eq!(weighted(&edges, 7), [0, 1, 1, 1, 2, 0, 2]);
    }

    #[test]
    fn a_node_moves_only_for_a_clearly_better_community() {
        let gain = |_: usize, inside: f64| inside;
        let even = BTreeMap::from([(1, 1.0), (2, 1.0)]);
        assert_eq!(best_community(5, &even, gain), 1);
        let with_own = BTreeMap::from([(1, 1.0), (5, 1.0)]);
        assert_eq!(best_community(5, &with_own, gain), 5);
        assert_eq!(best_community(5, &BTreeMap::from([(1, BETTER)]), gain), 5);
        let barely = BTreeMap::from([(1, 1.0), (2, 1.0 + BETTER / 2.0)]);
        assert_eq!(best_community(5, &barely, gain), 1);
        let clearly = BTreeMap::from([(1, 1.0), (2, 1.0 + BETTER * 4.0)]);
        assert_eq!(best_community(5, &clearly, gain), 2);
    }

    #[test]
    fn numbers_communities_by_their_first_node() {
        assert_eq!(renumber(&[7, 3, 7, 0]), [0, 1, 0, 2]);
    }

    #[test]
    fn aggregates_links_between_communities() {
        let graph = undirected(&links(&[(0, 1), (1, 2)], 3));
        let merged = aggregate(&graph, &[0, 0, 1]);
        assert_eq!(merged[0], BTreeMap::from([(0, 2.0), (1, 1.0)]));
        assert_eq!(merged[1], BTreeMap::from([(0, 1.0)]));
    }
}
