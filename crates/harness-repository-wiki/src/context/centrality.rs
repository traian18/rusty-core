//! How central each file is in the import graph: a small, deterministic PageRank over resolved
//! internal imports. Used to rank neighbours, never to decide that something is relevant.
use std::collections::HashMap;

use harness_workspace::repository_intelligence::{RepositorySnapshot, RootRelativePath};

const ITERATIONS: usize = 20;
const DAMPING: f64 = 0.85;

pub struct Centrality {
    scores: HashMap<RootRelativePath, f64>,
}

impl Centrality {
    pub fn new(snapshot: &RepositorySnapshot) -> Self {
        let files: Vec<&RootRelativePath> =
            snapshot.imports.files.iter().map(|f| &f.path).collect();
        let index: HashMap<&RootRelativePath, usize> =
            files.iter().enumerate().map(|(i, f)| (*f, i)).collect();
        let n = files.len().max(1);
        // An import makes the imported file more central: rank flows from importer to target.
        let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
        for edge in &snapshot.imports.edges {
            if let (Some(&from), Some(target)) = (
                index.get(&edge.fact.importing_file),
                edge.resolution.target_file(),
            ) {
                if let Some(&to) = index.get(target) {
                    if from != to {
                        outgoing[from].push(to);
                    }
                }
            }
        }
        let mut score = vec![1.0 / n as f64; files.len()];
        for _ in 0..ITERATIONS {
            let mut next = vec![(1.0 - DAMPING) / n as f64; files.len()];
            let mut sink = 0.0;
            for (from, targets) in outgoing.iter().enumerate() {
                if targets.is_empty() {
                    sink += score[from];
                } else {
                    let share = DAMPING * score[from] / targets.len() as f64;
                    for &to in targets {
                        next[to] += share;
                    }
                }
            }
            let spread = DAMPING * sink / n as f64;
            for value in &mut next {
                *value += spread;
            }
            score = next;
        }
        Self {
            scores: files.into_iter().cloned().zip(score).collect(),
        }
    }

    pub fn score(&self, file: &RootRelativePath) -> f64 {
        self.scores.get(file).copied().unwrap_or(0.0)
    }
}
