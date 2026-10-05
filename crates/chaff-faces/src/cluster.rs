//! Grouping faces into people.
//!
//! # What this produces, and what it does not
//!
//! A partition of face indices. It does **not** produce names — a cluster is a suggestion
//! that these faces might be one person, and confirming that is a human act (#46). Getting
//! this wrong costs a merge or a split in a review queue; getting it wrong *and* treating
//! the result as fact would put a stranger's name on a photograph.
//!
//! # Why Chinese Whispers
//!
//! The PRD names HDBSCAN. This is Chinese Whispers instead, for three reasons that matter
//! more here than cluster quality:
//!
//! * **It is deterministic.** A fixed iteration order and a fixed tie-break give the same
//!   partition every run. HDBSCAN's depends on which implementation and which parameters,
//!   and a library that re-clusters into a different shape on every launch is one nobody
//!   can correct — the correction is invalidated by the next run.
//! * **It needs no cluster count and no density estimate.** Both would be constants chosen
//!   against a corpus this does not have.
//! * **It is the algorithm dlib uses for exactly this**, which is some evidence that it is
//!   the right shape of answer for face grouping specifically.
//!
//! # Why the graph, and not every pair
//!
//! Two faces of one person are similar. Two faces of *different* people are usually not,
//! but occasionally are — a sibling, a similar haircut. Connecting every pair above a
//! threshold lets one such coincidence chain two groups together: A~B, B~C, and now A and C
//! are one person. Restricting each face to its **k nearest neighbours** means a spurious
//! link has to be among someone's closest matches, which is a much stronger claim.

use serde::{Deserialize, Serialize};

use crate::embed::cosine;

/// How to group faces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClusteringConfig {
    /// How many neighbours each face connects to.
    ///
    /// Low keeps groups tight and splits a person across several; high merges more readily.
    /// Eight is a compromise, and the review queue exists because neither is right for
    /// every library.
    pub neighbours: usize,
    /// The lowest similarity worth an edge.
    ///
    /// SFace's own operating point is around 0.36 for verification. Grouping is a different
    /// question — a false merge costs one correction in the review queue, a missed match
    /// costs a person being split across a dozen groups — so this sits slightly lower.
    pub min_similarity: f32,
    pub max_iterations: usize,
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self { neighbours: 8, min_similarity: 0.30, max_iterations: 20 }
    }
}

/// One group of faces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cluster {
    /// Indices into the input, ascending.
    pub members: Vec<usize>,
}

impl Cluster {
    pub fn len(&self) -> usize {
        self.members.len()
    }
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Group faces by similarity.
///
/// Returns clusters with **two or more** members, largest first. A face that matched
/// nothing is not a cluster of one — it is a face nobody else resembles, and returning it as
/// a group would fill the review queue with singletons that need no decision.
///
/// Indices in the result refer to positions in `embeddings`.
pub fn cluster(embeddings: &[Vec<f32>], config: &ClusteringConfig) -> Vec<Cluster> {
    if embeddings.len() < 2 {
        return Vec::new();
    }

    let graph = build_graph(embeddings, config);
    let labels = propagate(embeddings.len(), &graph, config.max_iterations);

    // Collect by label, then drop the singletons.
    let mut by_label: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for (face, label) in labels.iter().enumerate() {
        by_label.entry(*label).or_default().push(face);
    }

    let mut clusters: Vec<Cluster> = by_label
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| Cluster { members })
        .collect();

    // Largest first, then by first member so the order is total and stable. Two clusters of
    // the same size must not swap places between runs.
    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.members[0].cmp(&b.members[0]))
    });
    clusters
}

/// Adjacency, each entry `(neighbour, similarity)`.
type Graph = Vec<Vec<(usize, f32)>>;

/// Connect each face to its nearest neighbours above the threshold.
fn build_graph(embeddings: &[Vec<f32>], config: &ClusteringConfig) -> Graph {
    let n = embeddings.len();
    let mut graph: Graph = vec![Vec::new(); n];

    for i in 0..n {
        // Score against every later face, then add each direction. Half the work of scoring
        // every ordered pair, and the graph is symmetric by construction.
        let mut scored: Vec<(usize, f32)> = Vec::with_capacity(n - i - 1);
        for j in (i + 1)..n {
            let s = cosine(&embeddings[i], &embeddings[j]);
            if s >= config.min_similarity {
                scored.push((j, s));
            }
        }

        // Strongest first, ties broken by index so the graph does not depend on sort
        // stability. Then keep only the k nearest.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        scored.truncate(config.neighbours);

        for (j, s) in scored {
            graph[i].push((j, s));
            graph[j].push((i, s));
        }
    }

    // A neighbour list built by insertion is in whatever order the outer loop reached it,
    // which depends on i. Sorting makes the propagation order reproducible.
    for list in graph.iter_mut() {
        list.sort_by(|a, b| a.0.cmp(&b.0));
    }
    graph
}

/// Chinese Whispers: every node adopts the label most common among its neighbours.
fn propagate(n: usize, graph: &Graph, max_iterations: usize) -> Vec<usize> {
    // Start with every face its own class. The label values are the starting indices, so
    // the result is a function of the input alone — no hash map, no random seed.
    let mut labels: Vec<usize> = (0..n).collect();

    for _ in 0..max_iterations {
        let mut changed = 0usize;

        // A fixed order, and it matters: propagating in index order makes the outcome
        // reproducible, while a random or hash order would give a different partition on
        // every run. "It re-clustered differently today" is a bug report with no fix.
        for node in 0..n {
            if graph[node].is_empty() {
                continue;
            }

            // Weighted vote, ties broken by the lowest label so equal support does not
            // depend on iteration order within the map.
            let mut votes: std::collections::BTreeMap<usize, f32> = std::collections::BTreeMap::new();
            for (neighbour, weight) in &graph[node] {
                *votes.entry(labels[*neighbour]).or_insert(0.0) += *weight;
            }

            let best = votes
                .iter()
                .fold(None, |acc: Option<(usize, f32)>, (label, weight)| match acc {
                    Some((_, best_weight)) if *weight <= best_weight => acc,
                    _ => Some((*label, *weight)),
                })
                .map(|(label, _)| label);

            if let Some(best) = best {
                if labels[node] != best {
                    labels[node] = best;
                    changed += 1;
                }
            }
        }

        if changed == 0 {
            break;
        }
    }

    // Normalise to the smallest member index, so two runs that reached the same partition
    // by different paths produce the same labels. Without this, the *groups* would be
    // identical but the labels would differ, and any test comparing labels would fail
    // intermittently.
    let mut canonical: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for (node, label) in labels.iter_mut().enumerate() {
        let entry = canonical.entry(*label).or_insert(node);
        *label = *entry;
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit vector occupying its own block of dimensions, so different seeds are
    /// **orthogonal**.
    ///
    /// The first version used `sin((i + seed) * 0.7)`, which looks like it produces
    /// unrelated vectors and does not: `cos(base(0), base(100))` came out at **0.64**, well
    /// above the 0.30 threshold. The clustering merged them, correctly — the fixture had
    /// failed to create two different people, and the test blamed the algorithm.
    ///
    /// Distinct blocks make separation a property of the construction rather than of a
    /// constant that happens to work.
    fn unit(dim: usize, seed: f32) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        let block = (seed as usize) * 16;
        for k in 0..4 {
            v[(block + k) % dim] = 1.0 + k as f32 * 0.1;
        }
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= n;
        }
        v
    }

    #[test]
    fn the_test_fixtures_are_actually_distinct() {
        // **A test for the test.** The clustering tests only mean something if the "two
        // different people" in them are actually different, and the first version of `unit`
        // produced bases 0.64 similar while claiming to be unrelated.
        for a in 0..4 {
            for b in (a + 1)..4 {
                let s = cosine(&unit(128, a as f32), &unit(128, b as f32));
                assert!(
                    s.abs() < 0.01,
                    "fixture groups {a} and {b} are {s:.3} similar — they are not different people"
                );
            }
        }
    }

    /// A vector close to `base`, by blending in a little noise.
    fn near(base: &[f32], amount: f32, seed: f32) -> Vec<f32> {
        let noise = unit(base.len(), seed);
        let mut v: Vec<f32> = base
            .iter()
            .zip(noise.iter())
            .map(|(b, n)| b * (1.0 - amount) + n * amount)
            .collect();
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= n;
        }
        v
    }

    #[test]
    fn two_tight_groups_are_found_and_nothing_else_is() {
        let a = unit(128, 0.0);
        let b = unit(128, 100.0);
        let mut faces = Vec::new();
        for i in 0..5 {
            faces.push(near(&a, 0.02, i as f32));
            faces.push(near(&b, 0.02, 200.0 + i as f32));
        }

        let clusters = cluster(&faces, &ClusteringConfig::default());
        assert_eq!(clusters.len(), 2, "expected two people, got {clusters:?}");
        assert!(clusters.iter().all(|c| c.len() == 5), "each group has five faces");

        // The two groups must be exactly the two sets, not a mixture.
        let first: std::collections::BTreeSet<usize> = clusters[0].members.iter().copied().collect();
        let second: std::collections::BTreeSet<usize> = clusters[1].members.iter().copied().collect();
        let evens: std::collections::BTreeSet<usize> = (0..10).filter(|i| i % 2 == 0).collect();
        let odds: std::collections::BTreeSet<usize> = (0..10).filter(|i| i % 2 == 1).collect();
        assert!(
            (first == evens && second == odds) || (first == odds && second == evens),
            "the groups were mixed: {first:?} and {second:?}"
        );
    }

    #[test]
    fn a_face_that_resembles_nobody_is_not_a_cluster() {
        // A group of one is not a group. Returning singletons would fill the review queue
        // with faces that need no decision, which is how a review queue stops being used.
        let a = unit(128, 0.0);
        let mut faces = vec![near(&a, 0.01, 1.0), near(&a, 0.01, 2.0)];
        faces.push(unit(128, 500.0)); // unrelated

        let clusters = cluster(&faces, &ClusteringConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 2, "the unrelated face must be left out");
    }

    #[test]
    fn clustering_is_deterministic() {
        // **The property that makes corrections stick.** A library that re-clusters into a
        // different shape on every launch is one nobody can correct, because the correction
        // is invalidated by the next run.
        let mut faces = Vec::new();
        for g in 0..4 {
            let base = unit(128, g as f32 * 50.0);
            for i in 0..6 {
                faces.push(near(&base, 0.05, g as f32 * 100.0 + i as f32));
            }
        }

        let first = cluster(&faces, &ClusteringConfig::default());
        for _ in 0..5 {
            assert_eq!(cluster(&faces, &ClusteringConfig::default()), first);
        }
    }

    #[test]
    fn the_order_of_the_input_does_not_change_the_groups() {
        // The same faces in a different order must group the same way — a library re-indexed
        // in a different order cannot produce different people.
        let mut faces = Vec::new();
        for g in 0..3 {
            let base = unit(128, g as f32 * 80.0);
            for i in 0..4 {
                faces.push(near(&base, 0.03, g as f32 * 50.0 + i as f32));
            }
        }

        let forward = cluster(&faces, &ClusteringConfig::default());

        let mut order: Vec<usize> = (0..faces.len()).collect();
        order.reverse();
        let reversed: Vec<Vec<f32>> = order.iter().map(|i| faces[*i].clone()).collect();
        let backward = cluster(&reversed, &ClusteringConfig::default());

        // Sizes must match, because the groups are the same sets under a relabelling.
        let mut a: Vec<usize> = forward.iter().map(|c| c.len()).collect();
        let mut b: Vec<usize> = backward.iter().map(|c| c.len()).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "reversing the input changed the group sizes");
    }

    #[test]
    fn an_empty_or_single_face_yields_nothing() {
        assert!(cluster(&[], &ClusteringConfig::default()).is_empty());
        assert!(cluster(&[unit(128, 0.0)], &ClusteringConfig::default()).is_empty());
    }

    #[test]
    fn identical_embeddings_all_land_together() {
        // The degenerate but reachable case: several faces the recogniser cannot tell apart.
        // They are one group, not several, and certainly not a panic.
        let v = unit(128, 7.0);
        let faces = vec![v.clone(); 6];
        let clusters = cluster(&faces, &ClusteringConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 6);
    }

    #[test]
    fn a_zero_embedding_joins_nothing() {
        // A failed embedding compares as similar to nothing, so it must not become a group.
        // If `cosine` returned 1.0 for 0/0 — which several implementations do — every
        // failed face would cluster with every other and the symptom would be "all one
        // person".
        let zero = vec![0f32; 128];
        let a = unit(128, 0.0);
        let faces = vec![zero.clone(), zero, near(&a, 0.01, 1.0), near(&a, 0.01, 2.0)];

        let clusters = cluster(&faces, &ClusteringConfig::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members, vec![2, 3], "only the real faces grouped");
    }

    #[test]
    fn members_are_ascending_and_clusters_are_largest_first() {
        // The UI renders these in order; an unstable order is a list that reshuffles.
        let mut faces = Vec::new();
        for g in 0..3 {
            let base = unit(128, g as f32 * 60.0);
            for i in 0..(3 + g) {
                faces.push(near(&base, 0.03, g as f32 * 40.0 + i as f32));
            }
        }
        let clusters = cluster(&faces, &ClusteringConfig::default());
        for c in &clusters {
            assert!(c.members.windows(2).all(|w| w[0] < w[1]), "members must ascend: {c:?}");
        }
        assert!(
            clusters.windows(2).all(|w| w[0].len() >= w[1].len()),
            "largest first: {:?}",
            clusters.iter().map(|c| c.len()).collect::<Vec<_>>()
        );
    }
}
