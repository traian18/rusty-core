//! Near copies of functions across a repository. Functions whose syntax trees have the same
//! shape (node kinds and nesting, not names or literals) are grouped; each function then knows
//! the other places it exists.
use std::collections::{BTreeMap, HashMap};

use harness_code_analysis::metrics::FileMetrics;

use crate::{
    config::DuplicatedCodeConfig,
    context::{Duplicate, DuplicateSite},
};

#[derive(Clone)]
struct Site {
    path: String,
    function: String,
    start: u32,
    end: u32,
    nloc: u32,
}

#[derive(Default)]
pub struct DuplicateIndex {
    by_path: BTreeMap<String, Vec<Duplicate>>,
    /// Every comparable function by shape, so one edited file can be compared on its own.
    sites: HashMap<u64, Vec<Site>>,
}

/// Sorts a file's duplicates and drops closures that sit inside another copied function: they
/// are part of the same copy.
fn settle(list: &mut Vec<Duplicate>) {
    list.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then(a.function.cmp(&b.function))
    });
    for d in list.iter_mut() {
        d.copies
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    }
    let spans: Vec<(u32, u32)> = list.iter().map(|d| (d.start_line, d.end_line)).collect();
    let mut index = 0;
    list.retain(|_| {
        let me = spans[index];
        index += 1;
        !spans
            .iter()
            .any(|other| *other != me && other.0 <= me.0 && me.1 <= other.1)
    });
}

impl DuplicateIndex {
    /// `files` are `(path, metrics, is_test)`; tests neither count nor are reported, because
    /// repeated setup is how tests are written.
    pub fn build<'a>(
        files: impl IntoIterator<Item = (&'a str, &'a FileMetrics, bool)>,
        config: &DuplicatedCodeConfig,
    ) -> Self {
        let mut groups: HashMap<u64, Vec<Site>> = HashMap::new();
        for (path, metrics, test) in files {
            if test {
                continue;
            }
            for f in &metrics.functions {
                if f.shape == 0
                    || f.test
                    || f.nloc < config.min_nloc
                    || f.shape_nodes < config.min_nodes
                {
                    continue;
                }
                groups.entry(f.shape).or_default().push(Site {
                    path: path.to_owned(),
                    function: f.qualified_name.clone(),
                    start: f.start_line,
                    end: f.end_line,
                    nloc: f.nloc,
                });
            }
        }
        let mut by_path: BTreeMap<String, Vec<Duplicate>> = BTreeMap::new();
        for sites in groups.values().filter(|s| s.len() > 1) {
            for (i, site) in sites.iter().enumerate() {
                let copies = sites
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, other)| DuplicateSite {
                        path: other.path.clone(),
                        function: other.function.clone(),
                        line: other.start,
                    })
                    .collect();
                by_path
                    .entry(site.path.clone())
                    .or_default()
                    .push(Duplicate {
                        function: site.function.clone(),
                        start_line: site.start,
                        end_line: site.end,
                        nloc: site.nloc,
                        copies,
                    });
            }
        }
        for list in by_path.values_mut() {
            settle(list);
        }
        Self {
            by_path,
            sites: groups,
        }
    }

    /// The functions of `path` that have copies elsewhere, in line order.
    pub fn duplicates_in(&self, path: &str) -> &[Duplicate] {
        self.by_path.get(path).map_or(&[], Vec::as_slice)
    }

    /// The duplicates of `metrics` (a file as it is now) against the rest of the repository as
    /// the index last saw it; copies recorded for `path` itself are ignored, as they may be stale.
    pub fn duplicates_for(
        &self,
        path: &str,
        metrics: &FileMetrics,
        config: &DuplicatedCodeConfig,
    ) -> Vec<Duplicate> {
        let comparable = |f: &&harness_code_analysis::metrics::FunctionMetrics| {
            f.shape != 0
                && !f.test
                && f.nloc >= config.min_nloc
                && f.shape_nodes >= config.min_nodes
        };
        let mut out = Vec::new();
        for (i, f) in metrics.functions.iter().filter(comparable).enumerate() {
            let mut copies: Vec<DuplicateSite> = self
                .sites
                .get(&f.shape)
                .map(|sites| sites.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter(|site| site.path != path)
                .map(|site| DuplicateSite {
                    path: site.path.clone(),
                    function: site.function.clone(),
                    line: site.start,
                })
                .collect();
            copies.extend(
                metrics
                    .functions
                    .iter()
                    .filter(comparable)
                    .enumerate()
                    .filter(|(j, other)| *j != i && other.shape == f.shape)
                    .map(|(_, other)| DuplicateSite {
                        path: path.to_owned(),
                        function: other.qualified_name.clone(),
                        line: other.start_line,
                    }),
            );
            if !copies.is_empty() {
                out.push(Duplicate {
                    function: f.qualified_name.clone(),
                    start_line: f.start_line,
                    end_line: f.end_line,
                    nloc: f.nloc,
                    copies,
                });
            }
        }
        settle(&mut out);
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }
}
