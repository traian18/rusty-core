//! The offline retrieval replay: for recorded prompts whose runs are known to have read and edited
//! certain files, how well would each retrieval configuration have chosen them? No model is
//! involved, so it is deterministic and cheap, and it is how a configuration earns its place.
use std::{collections::HashSet, sync::Arc};

use harness_workspace::repository_intelligence::RepositorySnapshot;

use super::{
    centrality::Centrality,
    focus::{FocusSet, TaskKind},
    plan::{ContextPlanner, PlanInput, PlanOptions, Tier},
};
use crate::model::PageLevel;

/// One recorded request and what its run actually touched.
#[derive(Clone, Debug, Default)]
pub struct ReplayCase {
    pub prompt: String,
    /// Files the run read or edited.
    pub used: Vec<String>,
    pub task: TaskKind,
    pub focus: FocusSet,
}

/// A retrieval configuration to compare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Config {
    /// Only what the prompt names and what is in focus.
    AnchorsOnly,
    /// Plus identifiers that match file names.
    AnchorsAndNames,
    /// Plus neighbours, surroundings and task routing.
    Full,
}

impl Config {
    pub const ALL: [Config; 3] = [Config::AnchorsOnly, Config::AnchorsAndNames, Config::Full];

    fn options(self) -> PlanOptions {
        match self {
            Self::AnchorsOnly => PlanOptions {
                names: false,
                expand: false,
                route: false,
            },
            Self::AnchorsAndNames => PlanOptions {
                names: true,
                expand: false,
                route: false,
            },
            Self::Full => PlanOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayMetrics {
    pub cases: usize,
    /// Cases where only the overview was sent.
    pub abstained: usize,
    /// File excerpts injected, and how many of those files the run never touched (the drift measure).
    pub excerpts: usize,
    pub harmful: usize,
    /// Files the runs used, and how many were injected as excerpts, or at least as pointers.
    pub used: usize,
    pub found_as_excerpt: usize,
    pub found_as_pointer: usize,
    /// Runs that received at least one file excerpt, and how many of those used at least one of them.
    pub injected_runs: usize,
    pub useful_runs: usize,
}

impl ReplayMetrics {
    /// Share of injected file excerpts that the run used.
    pub fn precision(&self) -> f64 {
        if self.excerpts == 0 {
            1.0
        } else {
            (self.excerpts - self.harmful) as f64 / self.excerpts as f64
        }
    }

    /// Share of runs that were given an excerpt in which the run used at least one injected file.
    pub fn hit_rate(&self) -> f64 {
        if self.injected_runs == 0 {
            1.0
        } else {
            self.useful_runs as f64 / self.injected_runs as f64
        }
    }

    /// Share of used files that arrived as excerpts.
    pub fn recall_excerpt(&self) -> f64 {
        if self.used == 0 {
            1.0
        } else {
            self.found_as_excerpt as f64 / self.used as f64
        }
    }

    /// Share of used files that arrived as excerpts or pointers.
    pub fn recall_any(&self) -> f64 {
        if self.used == 0 {
            1.0
        } else {
            (self.found_as_excerpt + self.found_as_pointer) as f64 / self.used as f64
        }
    }
}

/// Replays `cases` against `snapshot` under one configuration.
pub fn replay(
    snapshot: &RepositorySnapshot,
    cases: &[ReplayCase],
    config: Config,
) -> ReplayMetrics {
    let planner = ContextPlanner::new(snapshot, Arc::new(Centrality::new(snapshot)))
        .with_options(config.options());
    let mut metrics = ReplayMetrics::default();
    for case in cases {
        let plan = planner.plan(&PlanInput {
            message: &case.prompt,
            focus: &case.focus,
            task: case.task,
        });
        let used: HashSet<&str> = case.used.iter().map(String::as_str).collect();
        metrics.cases += 1;
        metrics.used += used.len();
        if plan.abstained {
            metrics.abstained += 1;
            continue;
        }
        let file = |id: &str| id.strip_prefix("file:").map(str::to_owned);
        let excerpts: HashSet<String> = plan
            .items
            .iter()
            .filter(|i| {
                i.tier == Tier::High && i.page.parse().is_ok_and(|(l, _)| l == PageLevel::File)
            })
            .filter_map(|i| file(&i.page.0))
            .collect();
        let pointers: HashSet<String> = plan
            .items
            .iter()
            .filter(|i| i.tier == Tier::Medium)
            .filter_map(|i| file(&i.page.0))
            .collect();
        if !excerpts.is_empty() {
            metrics.injected_runs += 1;
            metrics.useful_runs += usize::from(excerpts.iter().any(|f| used.contains(f.as_str())));
        }
        metrics.excerpts += excerpts.len();
        metrics.harmful += excerpts
            .iter()
            .filter(|f| !used.contains(f.as_str()))
            .count();
        metrics.found_as_excerpt += used.iter().filter(|f| excerpts.contains(**f)).count();
        metrics.found_as_pointer += used
            .iter()
            .filter(|f| pointers.contains(**f) && !excerpts.contains(**f))
            .count();
    }
    metrics
}

/// Every configuration over the same cases, for comparison.
pub fn replay_all(
    snapshot: &RepositorySnapshot,
    cases: &[ReplayCase],
) -> Vec<(Config, ReplayMetrics)> {
    Config::ALL
        .iter()
        .map(|config| (*config, replay(snapshot, cases, *config)))
        .collect()
}
