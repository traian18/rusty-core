//! Questions a run asks a person, and their answers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A question a step puts to the user; the run waits until it is answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    /// Unique within the run.
    pub id: String,
    /// What is being asked, e.g. `approval` or `task_failure`, so a host can
    /// present it suitably.
    pub kind: String,
    pub prompt: String,
    /// The data the user decides on (a plan, a failure report, ...).
    #[serde(default)]
    pub subject: Value,
    /// The answers the user may give, in display order.
    pub decisions: Vec<InputDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDecision {
    pub id: String,
    pub label: String,
    /// The answer must carry text (e.g. the changes being asked for).
    #[serde(default)]
    pub requires_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResponse {
    /// One of the request's decision ids.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub by: Responder,
}

/// Who answered a question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Responder {
    #[default]
    User,
    /// The run was started with auto-approve.
    Auto,
}
