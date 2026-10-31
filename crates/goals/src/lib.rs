#![warn(clippy::all)]

//! Goal loops: an instruction and a result that counts as done, worked on in passes (each a
//! fresh agent run) until the result is reached, the budget runs out, or the agent is blocked.
//! A quality goal (targets on code health) is measured; a task goal (any other instruction) is
//! worked through a plan kept across passes, its checks, and the pass's strict judgement.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `contract` | The goal as saved: instruction, scope, targets, budget, status, journal |
//! | `measurement` | What one measurement of the scope found, and which targets it misses |
//! | `progress` | Whether a pass made things better, worse or changed nothing |
//! | `decision` | A measurement and the pass's report to the goal's next state |
//! | `prompt` | What the next pass is asked to do |
//!
//! No I/O: the host measures (code health, guidelines, checks) and runs the passes. The model
//! never ends a goal by saying so: a quality goal ends on the measurement, a task goal only once
//! its plan is finished and the checks it ran pass as well.
mod contract;
mod decision;
mod measurement;
mod progress;
mod prompt;

pub use contract::{
    Budget, Goal, GoalStatus, JournalEntry, PlanStep, StepStatus, Targets, Verdict,
    GOAL_SCHEMA_VERSION,
};
pub use decision::{decide, record, Decision, PassReport};
pub use measurement::{
    CheckOutcome, FileMeasure, FindingSummary, GuidelineSummary, Measurement, Severity, Unmet,
};
pub use progress::{broke, compare, Progress};
pub use prompt::{pass_prompt, status_text};

/// Where a workspace keeps its goals, relative to its root.
pub const GOALS_DIR: &str = ".rusty/goals";
