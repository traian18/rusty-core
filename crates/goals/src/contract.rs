//! The goal as it is saved in `.rusty/goals/<id>.json`.
use serde::{Deserialize, Serialize};

use crate::measurement::{Measurement, Severity};

pub const GOAL_SCHEMA_VERSION: u32 = 1;

/// Journal entries kept; older ones are dropped.
pub const JOURNAL_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub schema_version: u32,
    pub id: String,
    pub status: GoalStatus,
    /// What the user asked for, in their words.
    pub instruction: String,
    /// Workspace-relative files or globs the targets are measured on. Empty: the files the
    /// passes changed.
    #[serde(default)]
    pub scope: Vec<String>,
    /// The scope in the user's words (`scope="..."`): what the passes keep their work within. The
    /// measurement still runs on `scope`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_note: Option<String>,
    #[serde(default)]
    pub targets: Targets,
    #[serde(default)]
    pub budget: Budget,
    /// Undo a pass that made the measurement worse.
    #[serde(default = "default_true")]
    pub keep_best: bool,
    /// The chat the goal runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Passes run since the goal started or was last resumed.
    #[serde(default)]
    pub passes: u32,
    /// Passes in a row that changed nothing measurable.
    #[serde(default)]
    pub stalled: u32,
    /// The measurement before the first pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Measurement>,
    /// The latest accepted measurement: what the next pass starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Measurement>,
    /// Files the passes changed, for an empty scope.
    #[serde(default)]
    pub touched: Vec<String>,
    /// The plan as the last kept pass left it: what a task goal is worked through by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlanStep>,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
    /// Why the goal stopped, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

const fn default_true() -> bool {
    true
}

impl Goal {
    pub fn new(id: String, instruction: String, now: String) -> Self {
        Self {
            schema_version: GOAL_SCHEMA_VERSION,
            id,
            status: GoalStatus::Running,
            instruction,
            scope: Vec::new(),
            scope_note: None,
            targets: Targets::default(),
            budget: Budget::default(),
            keep_best: true,
            chat_id: None,
            created_at: now.clone(),
            updated_at: now,
            passes: 0,
            stalled: 0,
            baseline: None,
            last: None,
            touched: Vec::new(),
            plan: Vec::new(),
            journal: Vec::new(),
            reason: None,
        }
    }

    pub fn push_journal(&mut self, entry: JournalEntry) {
        self.journal.push(entry);
        if self.journal.len() > JOURNAL_LIMIT {
            let excess = self.journal.len() - JOURNAL_LIMIT;
            self.journal.drain(..excess);
        }
    }

    /// A task goal: no target on the files' code health, so it is worked through a plan and
    /// judged in words. A quality goal is measured.
    pub fn is_task(&self) -> bool {
        !self.targets.on_files()
    }

    /// The result in words the pass judges: the one given, or for a task goal its instruction.
    pub fn done_when(&self) -> Option<&str> {
        match &self.targets.done_when {
            Some(words) => Some(words.as_str()),
            None if self.is_task() => Some(self.instruction.as_str()),
            None => None,
        }
    }

    /// Resumes a paused or blocked goal with a fresh pass budget.
    pub fn resume(&mut self, now: String) {
        self.status = GoalStatus::Running;
        self.passes = 0;
        self.stalled = 0;
        self.reason = None;
        self.updated_at = now;
    }
}

/// One step of a goal's plan, as the pass's to-do list had it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub content: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl StepStatus {
    /// Completed or cancelled: nothing left to do for it.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Running,
    Paused,
    Done,
    Blocked,
    Cleared,
}

impl GoalStatus {
    pub fn is_active(self) -> bool {
        self == Self::Running
    }
}

/// What counts as done. Every target set must be met. Without a target on the files the goal is a
/// task goal (see [`Goal::is_task`]): done when its plan is finished and the pass judges the
/// instruction met.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Targets {
    /// Every file in scope scores at least this (code health, 1-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_file_health: Option<f64>,
    /// The mean score of the files in scope is at least this ("general health").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_mean_health: Option<f64>,
    /// No code health finding in scope is more severe than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_severity: Option<Severity>,
    /// Checks (`typecheck`, `lint`, `test`, `build`) that must pass.
    #[serde(default)]
    pub checks: Vec<String>,
    /// The guidelines report no errors in scope.
    #[serde(default = "default_true")]
    pub guidelines: bool,
    /// A result in words; judged by a model only once every measured target is met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<String>,
}

impl Default for Targets {
    fn default() -> Self {
        Self {
            min_file_health: None,
            min_mean_health: None,
            max_severity: None,
            checks: Vec::new(),
            guidelines: true,
            done_when: None,
        }
    }
}

impl Targets {
    /// A target measured on the files' code health, which an empty measurement cannot meet.
    pub fn on_files(&self) -> bool {
        self.min_file_health.is_some()
            || self.min_mean_health.is_some()
            || self.max_severity.is_some()
    }

    /// The score below which a file is still work: the higher of the minimum and the mean target
    /// (a mean is raised by raising the files below it).
    pub fn work_bar(&self) -> Option<f64> {
        match (self.min_file_health, self.min_mean_health) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Passes before the goal pauses (`loops=`); resuming starts a fresh count. Without one the
    /// goal runs until it is done, blocked, or stops making progress.
    #[serde(default)]
    pub max_passes: Option<u32>,
    /// Passes in a row without measurable progress before the goal pauses.
    #[serde(default = "default_no_progress")]
    pub no_progress_passes: u32,
}

fn default_no_progress() -> u32 {
    Budget::default().no_progress_passes
}

impl Budget {
    /// Passes left after `used`, when there is a limit.
    pub fn passes_left(&self, used: u32) -> Option<u32> {
        self.max_passes.map(|max| max.saturating_sub(used).max(1))
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_passes: None,
            no_progress_passes: 2,
        }
    }
}

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every target is met.
    Done,
    /// Progress, or at least no harm: the next pass goes on from here.
    Continue,
    /// The pass made things worse and is undone.
    Undone,
    /// Nothing measurable changed for too many passes.
    NoProgress,
    /// The agent reported a blocker only the user can remove.
    Blocked,
    /// The pass budget is spent.
    Budget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub pass: u32,
    pub at: String,
    pub verdict: Verdict,
    /// One line: what the measurement says now, against before.
    pub summary: String,
    /// The agent's own status line, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    /// What the agent left for the next pass (its `Next:` line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}
