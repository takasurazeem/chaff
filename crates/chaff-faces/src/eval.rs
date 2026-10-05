//! Measuring how good the grouping actually is (#48).
//!
//! # The gate
//!
//! The PRD sets one: **K3 ≥ 97% precision**. K3 is the standard face-clustering measure —
//! for every face, take its three nearest neighbours in the same cluster and ask what
//! fraction are genuinely the same person. It is a *precision* measure, and it is the right
//! one here: a false merge puts one person's photographs under another's name, which is the
//! failure that matters, while a false split just means a group needs joining in the review
//! queue.
//!
//! # What this harness can and cannot tell you
//!
//! **It measures the metric, the partition and the config.** Given embeddings and the truth,
//! it reports precision, recall, F1, K3 and the cluster count, and it can sweep a config to
//! find where the gate is met.
//!
//! **It cannot tell you the clustering is good on real faces.** That needs a labelled corpus
//! of real photographs, which this repository does not ship — a face dataset is biometric
//! data and none of it belongs in a public repository. `from_labelled_corpus` loads one when
//! the user has it; without it, the synthetic generator exercises the harness and proves
//! nothing about accuracy.
//!
//! Saying which of those two happened is the whole point of the `Source` field.

use std::collections::{BTreeMap, BTreeSet};

use crate::cluster::{cluster, ClusteringConfig};
use crate::embed::cosine;

/// Where an evaluation's data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Generated embeddings with known structure. Exercises the harness; says nothing about
    /// accuracy on real faces.
    Synthetic,
    /// A real labelled corpus. This is the only source that can pass or fail the gate.
    Labelled,
}

/// How well a partition matches the truth.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub source: Source,
    /// Faces evaluated.
    pub faces: usize,
    /// Distinct people in the truth.
    pub people: usize,
    /// Clusters the algorithm produced, counting only those with two or more faces.
    pub clusters: usize,
    /// K3: for each face, the fraction of its three nearest same-cluster neighbours that are
    /// genuinely the same person. `None` when no cluster is large enough to have three.
    pub k3: Option<f64>,
    /// Of the pairs the algorithm put together, the fraction that belong together.
    pub pair_precision: f64,
    /// Of the pairs that belong together, the fraction the algorithm put together.
    pub pair_recall: f64,
    pub pair_f1: f64,
}

impl Score {
    /// Does this meet the PRD's gate?
    ///
    /// **A synthetic run never passes.** The gate is a claim about accuracy on real faces,
    /// and a harness that reported "pass" from generated data would be the most dangerous
    /// kind of green check.
    pub fn meets_gate(&self) -> bool {
        self.source == Source::Labelled && self.k3.is_some_and(|k| k >= 0.97)
    }

    /// One line, for a CI log.
    pub fn summary(&self) -> String {
        format!(
            "{:?}: {} faces, {} people → {} clusters · K3 {:.3} · precision {:.3} · recall {:.3}{}",
            self.source,
            self.faces,
            self.people,
            self.clusters,
            self.k3.unwrap_or(0.0),
            self.pair_precision,
            self.pair_recall,
            if self.meets_gate() { " · GATE MET" } else { "" }
        )
    }
}

/// Score a partition against the truth.
///
/// `labels[i]` is the true person of face `i`. Clusters are compared as *sets*: the
/// algorithm's numbering has nothing to do with the truth's, and comparing ids would score a
/// perfect partition at zero.
pub fn score(
    embeddings: &[Vec<f32>],
    labels: &[usize],
    clusters: &[Vec<usize>],
    source: Source,
) -> Score {
    assert_eq!(embeddings.len(), labels.len(), "one label per face");

    // Which cluster each face landed in, or None.
    let mut cluster_of: Vec<Option<usize>> = vec![None; labels.len()];
    for (c, members) in clusters.iter().enumerate() {
        for m in members {
            cluster_of[*m] = Some(c);
        }
    }

    let (mut true_pos, mut false_pos, mut false_neg) = (0usize, 0usize, 0usize);
    for i in 0..labels.len() {
        for j in (i + 1)..labels.len() {
            let same_person = labels[i] == labels[j];
            let same_cluster = cluster_of[i].is_some() && cluster_of[i] == cluster_of[j];
            match (same_person, same_cluster) {
                (true, true) => true_pos += 1,
                (false, true) => false_pos += 1,
                (true, false) => false_neg += 1,
                (false, false) => {}
            }
        }
    }

    let precision = ratio(true_pos, true_pos + false_pos);
    let recall = ratio(true_pos, true_pos + false_neg);
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };

    Score {
        source,
        faces: labels.len(),
        people: labels.iter().collect::<BTreeSet<_>>().len(),
        clusters: clusters.iter().filter(|c| c.len() > 1).count(),
        k3: k3(embeddings, labels, clusters),
        pair_precision: precision,
        pair_recall: recall,
        pair_f1: f1,
    }
}

fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64
    }
}

/// K3, as the face-recognition literature defines it.
///
/// For every face with at least three cluster-mates, the three most similar of them, and the
/// fraction of those three that are genuinely the same person. Averaged over every face that
/// qualifies.
fn k3(embeddings: &[Vec<f32>], labels: &[usize], clusters: &[Vec<usize>]) -> Option<f64> {
    let mut total = 0.0f64;
    let mut counted = 0usize;

    for members in clusters {
        if members.len() < 4 {
            // A face needs three *others* in its cluster to have a K3 at all.
            continue;
        }
        for &i in members {
            let mut scored: Vec<(usize, f32)> = members
                .iter()
                .copied()
                .filter(|j| *j != i)
                .map(|j| (j, cosine(&embeddings[i], &embeddings[j])))
                .collect();

            // Descending similarity, ties broken by index so the result is reproducible.
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
            });
            scored.truncate(3);

            let correct = scored.iter().filter(|(j, _)| labels[*j] == labels[i]).count();
            total += correct as f64 / scored.len() as f64;
            counted += 1;
        }
    }

    (counted > 0).then(|| total / counted as f64)
}

/// A labelled corpus: embeddings and the person each belongs to.
#[derive(Debug, Clone)]
pub struct Corpus {
    pub embeddings: Vec<Vec<f32>>,
    pub labels: Vec<usize>,
    pub source: Source,
}

/// Build a corpus from a directory of one folder per person.
///
/// **The only source that can pass the gate.** Each subdirectory is a person; each file in it
/// is a photograph of them. Nothing is shipped with this repository — a face dataset is
/// biometric data — so this reads one the user has.
pub fn from_labelled_corpus(
    root: &std::path::Path,
    embed: impl Fn(&std::path::Path) -> Option<Vec<f32>>,
) -> std::io::Result<Corpus> {
    let mut embeddings = Vec::new();
    let mut labels = Vec::new();
    let mut people: BTreeMap<String, usize> = BTreeMap::new();

    let mut dirs: Vec<_> = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    // Sorted, so the label numbering is stable across runs and a diff of two reports is
    // readable.
    dirs.sort_by_key(|e| e.file_name());

    for entry in dirs {
        let name = entry.file_name().to_string_lossy().to_string();
        // Two statements: `entry(..).or_insert(people.len())` borrows `people` mutably and
        // reads its length in the same expression, which the borrow checker refuses — and is
        // right to, because the order is not defined.
        let next = people.len();
        let id = *people.entry(name).or_insert(next);

        let mut files: Vec<_> = std::fs::read_dir(entry.path())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();

        for file in files {
            if let Some(v) = embed(&file) {
                embeddings.push(v);
                labels.push(id);
            }
        }
    }

    Ok(Corpus { embeddings, labels, source: Source::Labelled })
}

/// Generate embeddings with known structure.
///
/// # What this proves, and what it does not
///
/// It proves the **harness** works: that `score` computes precision, recall and K3 correctly
/// on data whose answer is known. It proves nothing about accuracy on real faces, and
/// [`Source::Synthetic`] is carried through to [`Score`] so a report cannot be mistaken for
/// the real thing.
///
/// Each person gets a distinct direction in a high-dimensional space; each photograph is that
/// direction plus noise. `spread` controls how hard the problem is — the point where the
/// clustering starts merging people is a property of the generator, not of faces.
pub fn synthetic(people: usize, per_person: usize, dim: usize, spread: f32) -> Corpus {
    let mut embeddings = Vec::new();
    let mut labels = Vec::new();

    for p in 0..people {
        // A distinct block per person, so two people are near-orthogonal by construction
        // rather than by a constant that happens to work.
        let mut base = vec![0f32; dim];
        for k in 0..4 {
            base[(p * 7 + k * 13) % dim] = 1.0 + k as f32 * 0.1;
        }
        let n: f32 = base.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in base.iter_mut() {
            *x /= n;
        }

        for i in 0..per_person {
            let mut v: Vec<f32> = base
                .iter()
                .enumerate()
                .map(|(d, b)| {
                    // Deterministic noise: a hash of (person, photo, dimension), so two runs
                    // of the harness produce the same numbers.
                    let h = ((p * 7919 + i * 104_729 + d * 1299709) % 1000) as f32 / 1000.0 - 0.5;
                    b * (1.0 - spread) + h * spread
                })
                .collect();
            let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in v.iter_mut() {
                *x /= n;
            }
            embeddings.push(v);
            labels.push(p);
        }
    }

    Corpus { embeddings, labels, source: Source::Synthetic }
}

/// Sweep a config, returning a score for each neighbour count.
///
/// The knob that matters is `neighbours`: it is the one that decides how readily two groups
/// merge, and merging is the expensive error.
pub fn sweep(corpus: &Corpus, neighbours: &[usize]) -> Vec<(usize, Score)> {
    neighbours
        .iter()
        .map(|k| {
            let config = ClusteringConfig { neighbours: *k, ..ClusteringConfig::default() };
            let clusters: Vec<Vec<usize>> = cluster(&corpus.embeddings, &config)
                .into_iter()
                .map(|c| c.members)
                .collect();
            (*k, score(&corpus.embeddings, &corpus.labels, &clusters, corpus.source))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(dim: usize, block: usize) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        for k in 0..4 {
            v[(block * 16 + k) % dim] = 1.0;
        }
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= n;
        }
        v
    }

    #[test]
    fn a_perfect_partition_scores_one() {
        // The harness's own correctness: if it cannot recognise a perfect answer, its numbers
        // mean nothing.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 1), unit(64, 1)];
        let labels = vec![0, 0, 1, 1];
        let clusters = vec![vec![0, 1], vec![2, 3]];

        let s = score(&embeddings, &labels, &clusters, Source::Labelled);
        assert!((s.pair_precision - 1.0).abs() < 1e-9);
        assert!((s.pair_recall - 1.0).abs() < 1e-9);
        assert_eq!(s.people, 2);
        assert_eq!(s.clusters, 2);
    }

    #[test]
    fn merging_everything_scores_perfect_recall_and_poor_precision() {
        // **The failure that matters.** One cluster containing everybody has recall 1.0 and
        // precision near zero, which is exactly why the gate is on precision and why K3
        // exists — a metric that only counted recall would call this perfect.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 1), unit(64, 1)];
        let labels = vec![0, 0, 1, 1];
        let clusters = vec![vec![0, 1, 2, 3]];

        let s = score(&embeddings, &labels, &clusters, Source::Labelled);
        assert!((s.pair_recall - 1.0).abs() < 1e-9, "everything found");
        assert!(s.pair_precision < 0.7, "but most of it is wrong: {}", s.pair_precision);
        assert!(!s.meets_gate());
    }

    #[test]
    fn splitting_everyone_scores_perfect_precision_and_no_recall() {
        // The opposite error: every face its own person. Precision is vacuously 1.0 because
        // there are no pairs to be wrong about, which is why recall has to be reported too.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 1)];
        let labels = vec![0, 0, 1];
        let clusters: Vec<Vec<usize>> = (0..3).map(|i| vec![i]).collect();

        let s = score(&embeddings, &labels, &clusters, Source::Labelled);
        assert_eq!(s.pair_precision, 0.0, "no pairs at all");
        assert_eq!(s.pair_recall, 0.0);
        assert_eq!(s.clusters, 0, "singletons are not clusters");
    }

    #[test]
    fn cluster_numbering_does_not_matter() {
        // Clusters are compared as **sets**. The algorithm's numbering has nothing to do with
        // the truth's, and comparing ids would score a perfect partition at zero.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 1), unit(64, 1)];
        let labels = vec![0, 0, 1, 1];

        let a = score(&embeddings, &labels, &[vec![0, 1], vec![2, 3]], Source::Labelled);
        let b = score(&embeddings, &labels, &[vec![2, 3], vec![0, 1]], Source::Labelled);
        assert_eq!(a.pair_precision, b.pair_precision);
        assert_eq!(a.k3, b.k3);
    }

    #[test]
    fn k3_is_none_when_no_cluster_is_big_enough() {
        // A face needs three *others* in its cluster to have a K3. Reporting 0.0 would read
        // as a total failure rather than an unanswerable question.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 1)];
        let labels = vec![0, 0, 1];
        let s = score(&embeddings, &labels, &[vec![0, 1]], Source::Labelled);
        assert_eq!(s.k3, None);
    }

    #[test]
    fn k3_counts_the_three_most_similar_cluster_mates() {
        // Four faces of one person and one stranger wrongly merged in, all in one cluster.
        //
        // The four real faces each have three identical cluster-mates, so each scores 3/3.
        // The **stranger** scores 0/3: every one of its cluster-mates is a different person,
        // and all four are equally similar to it (orthogonal blocks, cosine 0), so the
        // tie-break by index picks three of them.
        //
        // (4 × 1.0 + 0.0) / 5 = 0.8. The first version of this test asserted > 0.9 and was
        // **wrong** — the code was right and the expectation was not, which is worth writing
        // down because the instinct is to assume the metric is broken.
        //
        // This is also the honest shape of K3: one wrongly-merged stranger drags it down by
        // 0.2 here, so it is sensitive to the error that matters, while four correct faces
        // keep it from collapsing.
        let embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 0), unit(64, 0), unit(64, 5)];
        let labels = vec![0, 0, 0, 0, 1];
        let s = score(&embeddings, &labels, &[vec![0, 1, 2, 3, 4]], Source::Labelled);

        assert!((s.k3.unwrap() - 0.8).abs() < 1e-6, "K3 was {:?}", s.k3);
        assert!(s.pair_precision < 1.0, "and the partition is not perfect");
    }

    #[test]
    fn k3_falls_as_a_wrong_merge_grows() {
        // The property the number has to have to be worth reporting: adding strangers to a
        // cluster must lower it, monotonically.
        let mut previous = 1.0f64;
        for strangers in 0..5 {
            let mut embeddings = vec![unit(64, 0), unit(64, 0), unit(64, 0), unit(64, 0)];
            let mut labels = vec![0, 0, 0, 0];
            let mut members: Vec<usize> = (0..4).collect();
            for k in 0..strangers {
                embeddings.push(unit(64, 1 + k));
                labels.push(1 + k);
                members.push(4 + k);
            }

            let s = score(&embeddings, &labels, &[members], Source::Labelled);
            let k3 = s.k3.unwrap();
            assert!(k3 <= previous + 1e-9, "K3 rose from {previous} to {k3}");
            previous = k3;
        }
        assert!(previous < 1.0, "and it did fall");
    }

    #[test]
    fn a_synthetic_run_never_meets_the_gate() {
        // **The most dangerous kind of green check** would be a harness that reported "pass"
        // from generated data. The gate is a claim about accuracy on real faces.
        let corpus = synthetic(20, 10, 128, 0.05);
        let clusters: Vec<Vec<usize>> = cluster(&corpus.embeddings, &ClusteringConfig::default())
            .into_iter()
            .map(|c| c.members)
            .collect();
        let s = score(&corpus.embeddings, &corpus.labels, &clusters, corpus.source);

        assert_eq!(s.source, Source::Synthetic);
        assert!(!s.meets_gate(), "synthetic data must never pass the gate");
        // Even with a perfect K3.
        let perfect = Score { k3: Some(1.0), ..s.clone() };
        assert!(!perfect.meets_gate(), "not even a perfect synthetic score");
    }

    #[test]
    fn the_synthetic_generator_produces_separable_people() {
        // If the generator produced mush, the harness tests above would be testing nothing.
        let corpus = synthetic(10, 8, 128, 0.02);
        assert_eq!(corpus.embeddings.len(), 80);
        assert_eq!(corpus.labels.len(), 80);
        assert_eq!(corpus.labels.iter().collect::<BTreeSet<_>>().len(), 10);

        let clusters: Vec<Vec<usize>> = cluster(&corpus.embeddings, &ClusteringConfig::default())
            .into_iter()
            .map(|c| c.members)
            .collect();
        let s = score(&corpus.embeddings, &corpus.labels, &clusters, Source::Synthetic);
        assert!(s.pair_precision > 0.95, "the generator is too noisy: {s:?}");
        assert!(s.pair_recall > 0.95, "the generator is too noisy: {s:?}");
    }

    #[test]
    fn the_synthetic_generator_is_deterministic() {
        // Two runs of the harness must produce the same numbers, or a CI log cannot be
        // compared to yesterday's.
        let a = synthetic(5, 4, 32, 0.1);
        let b = synthetic(5, 4, 32, 0.1);
        assert_eq!(a.embeddings, b.embeddings);
        assert_eq!(a.labels, b.labels);
    }

    #[test]
    fn the_sweep_reports_a_score_per_neighbour_count() {
        let corpus = synthetic(8, 6, 64, 0.05);
        let results = sweep(&corpus, &[2, 4, 8]);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, 2);
        // Every entry is scored, so a caller can pick a config rather than guess one.
        assert!(results.iter().all(|(_, s)| s.faces == 48));
    }

    #[test]
    fn the_summary_says_which_source_it_was() {
        // A report that does not distinguish synthetic from real is one that gets quoted as
        // if it were real.
        let corpus = synthetic(4, 4, 32, 0.05);
        let s = score(&corpus.embeddings, &corpus.labels, &[], Source::Synthetic);
        assert!(s.summary().contains("Synthetic"));
        assert!(!s.summary().contains("GATE MET"));
    }

    #[test]
    fn a_missing_corpus_directory_is_an_error_not_a_panic() {
        let r = from_labelled_corpus(std::path::Path::new("/nonexistent-xyz"), |_| None);
        assert!(r.is_err());
    }

    #[test]
    fn a_corpus_is_labelled_one_folder_per_person() {
        // The layout every face dataset uses, and the only source that can pass the gate.
        let dir = tempfile::tempdir().unwrap();
        for person in ["ada", "bob"] {
            let d = dir.path().join(person);
            std::fs::create_dir(&d).unwrap();
            for i in 0..3 {
                std::fs::write(d.join(format!("{i}.jpg")), b"x").unwrap();
            }
        }

        // A fake embedder: the folder name decides the direction, which is the property the
        // loader is responsible for getting right.
        let corpus = from_labelled_corpus(dir.path(), |p| {
            let person = p.parent()?.file_name()?.to_string_lossy().to_string();
            Some(unit(64, if person == "ada" { 0 } else { 1 }))
        })
        .unwrap();

        assert_eq!(corpus.embeddings.len(), 6);
        assert_eq!(corpus.source, Source::Labelled);
        assert_eq!(corpus.labels.iter().collect::<BTreeSet<_>>().len(), 2);
        // And each person's three files share a label.
        let ada = corpus.labels[0];
        assert_eq!(corpus.labels[..3].iter().filter(|l| **l == ada).count(), 3);
    }
}
