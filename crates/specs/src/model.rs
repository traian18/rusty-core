//! Plain data: what the parsers read and what the overview reports to the page.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Draft,
    Specified,
    Planned,
    Building,
    Verifying,
    Done,
    Archived,
}

impl Status {
    /// Lenient: unknown words are `None` so the caller can warn.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word.trim().to_ascii_lowercase().as_str() {
            "draft" => Self::Draft,
            "specified" => Self::Specified,
            "planned" => Self::Planned,
            "building" => Self::Building,
            "verifying" => Self::Verifying,
            "done" => Self::Done,
            "archived" => Self::Archived,
            _ => return None,
        })
    }
    pub fn word(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Specified => "specified",
            Self::Planned => "planned",
            Self::Building => "building",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Archived => "archived",
        }
    }
    /// The workflow that moves a change on from this status.
    pub fn next_step(self) -> Option<NextStep> {
        match self {
            Self::Draft => Some(NextStep::Specify),
            Self::Specified => Some(NextStep::Plan),
            Self::Planned | Self::Building | Self::Verifying => Some(NextStep::Implement),
            Self::Done => Some(NextStep::Archive),
            Self::Archived => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    Specify,
    Plan,
    Implement,
    Archive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Note,
    Warning,
    Error,
}

/// A problem with a document, with the line to open when it has one.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Warning {
    pub file: String,
    pub line: Option<usize>,
    pub severity: Severity,
    pub message: String,
}

impl Warning {
    pub fn new(
        file: &str,
        line: Option<usize>,
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file: file.to_owned(),
            line,
            severity,
            message: message.into(),
        }
    }
}

/// `change.md` front matter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeFront {
    pub id: Option<String>,
    pub title: Option<String>,
    pub status: Option<Status>,
    /// The raw word when it is not a known status.
    pub unknown_status: Option<String>,
    pub created: Option<String>,
    pub branch: Option<String>,
    pub capabilities: Vec<String>,
    /// For a change linked across repositories: the repository (relative to the workspace) that
    /// holds the whole change's spec.
    pub main: Option<String>,
    /// The other parts of a linked change, as `<repository>/<change folder>`.
    pub links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub id: String,
    pub text: String,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Requirement {
    pub id: String,
    pub title: String,
    pub line: usize,
    /// The `Capability:` line as written.
    pub capability: Option<String>,
    /// The prose under the heading, without the capability line and the criteria.
    pub detail: String,
    pub criteria: Vec<Criterion>,
}

/// A change's `spec.md`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpecDoc {
    pub title: Option<String>,
    pub sections: Vec<String>,
    pub requirements: Vec<Requirement>,
    /// Unanswered questions: (line, text).
    pub open_questions: Vec<(usize, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub line: usize,
    pub requirements: Vec<String>,
    pub criteria: Vec<String>,
    pub after: Vec<String>,
    pub instructions: String,
}

/// A change's `tasks.md`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TasksDoc {
    pub tasks: Vec<Task>,
}

/// A capability's `spec.md`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CapabilityDoc {
    pub title: Option<String>,
    pub purpose: Option<String>,
    pub requirements: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySummary {
    pub name: String,
    pub title: String,
    pub purpose: Option<String>,
    pub requirements: usize,
    pub path: String,
    /// Where the text has drifted from the code (named files that do not exist).
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
    pub dir: String,
    pub number: Option<u32>,
    pub title: String,
    pub status: Status,
    pub created: Option<String>,
    pub branch: Option<String>,
    /// See [`ChangeFront::main`] and [`ChangeFront::links`].
    pub main: Option<String>,
    pub links: Vec<String>,
    pub tasks_done: usize,
    pub tasks_total: usize,
    pub open_questions: usize,
    /// The fixed documents that exist, in reading order.
    pub documents: Vec<String>,
    /// File names under `tasks/`.
    pub task_documents: Vec<String>,
    pub next_step: Option<NextStep>,
    /// Set when the files say something the status does not, e.g. every task is ticked.
    pub hint: Option<String>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedChange {
    pub dir: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecsOverview {
    /// The specs root, relative to the repository.
    pub root: String,
    /// Whether anything exists under the root yet.
    pub exists: bool,
    pub project: bool,
    pub project_warnings: Vec<Warning>,
    pub capabilities: Vec<CapabilitySummary>,
    /// The capability list of an onboarding in progress (`onboarding.md`); absent when there is none.
    pub onboarding: Option<Vec<crate::onboarding::Entry>>,
    pub changes: Vec<ChangeSummary>,
    pub archive: Vec<ArchivedChange>,
    /// Problems that belong to no single change (duplicate numbers, odd folder names).
    pub warnings: Vec<Warning>,
}
