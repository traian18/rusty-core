//! Cleaning up what runs leave in a workspace.
//!
//! Sessions, run stores and subagent worktrees accumulate as runs go by, and a long-lived
//! workspace gets slower for it. [`scan`] looks and reports what is old, how big it is and where it
//! is, and changes nothing; [`apply`] removes exactly the candidates it is given. A host shows the
//! report first and applies only after the user confirms.

mod apply;
mod scan;

use std::path::PathBuf;
use std::time::Duration;

pub use apply::{apply, Removed};
pub use scan::scan;

/// What kind of leftover a candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A stored session (`.harness/sessions`).
    Session,
    /// A stored workflow run (a folder a host keeps run stores in).
    RunStore,
    /// A subagent's git worktree (`.harness-worktrees`).
    Worktree,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::RunStore => "run store",
            Self::Worktree => "worktree",
        }
    }
}

/// How old each kind must be, untouched, before it is a candidate.
#[derive(Debug, Clone)]
pub struct Policy {
    pub sessions: Duration,
    pub run_stores: Duration,
    pub worktrees: Duration,
    /// Folders whose entries are workflow run stores (the host knows where it keeps them).
    pub run_store_roots: Vec<PathBuf>,
}

impl Default for Policy {
    fn default() -> Self {
        let days = |n: u64| Duration::from_secs(n * 86_400);
        Self {
            sessions: days(30),
            run_stores: days(30),
            worktrees: days(7),
            run_store_roots: Vec::new(),
        }
    }
}

/// One thing that could be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    pub path: PathBuf,
    pub bytes: u64,
    /// Days since anything in it was last changed.
    pub idle_days: u64,
}

/// Everything [`scan`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub candidates: Vec<Candidate>,
}

impl Report {
    pub fn total_bytes(&self) -> u64 {
        self.candidates.iter().map(|c| c.bytes).sum()
    }

    /// A readable table, oldest and biggest first, with a total.
    pub fn render(&self) -> String {
        if self.candidates.is_empty() {
            return "nothing old enough to clean up".to_owned();
        }
        let mut lines: Vec<String> = self
            .candidates
            .iter()
            .map(|c| {
                format!(
                    "{:<10} {:>9} {:>5}d idle  {}",
                    c.kind.label(),
                    human_bytes(c.bytes),
                    c.idle_days,
                    c.path.display()
                )
            })
            .collect();
        lines.push(format!(
            "{} item(s), {} in total",
            self.candidates.len(),
            human_bytes(self.total_bytes())
        ));
        lines.join("\n")
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests;
