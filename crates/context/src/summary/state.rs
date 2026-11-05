//! What the summarizing provider remembers between requests: summaries in flight and finished.

use std::collections::{HashMap, HashSet};

use std::sync::{Arc, Mutex};

use rusty_protocol::ids::MessageId;
use rusty_protocol::messages::AgentMessage;

#[derive(Default)]
pub(super) struct SummaryState {
    pub(super) telemetry: Option<crate::observability::CompactionTelemetry>,
    pub(super) summary: Option<String>,
    /// Last message the summary (and verdicts) account for.
    pub(super) covered_through: Option<MessageId>,
    pub(super) essential: HashSet<MessageId>,
    pub(super) disposable: HashSet<MessageId>,
    pub(super) judged: HashSet<MessageId>,
    pub(super) in_flight: bool,
}

/// Summary state per conversation, keyed by the conversation's first message.
/// Sub-agents and workflow steps inherit the session's backend, so one
/// provider sees several conversations and must never mix their summaries.
pub(super) type States = Arc<Mutex<HashMap<MessageId, SummaryState>>>;

/// Clears the conversation's in-flight flag when the background task ends,
/// however it ends.
pub(super) struct Flight {
    pub(super) states: States,
    pub(super) key: MessageId,
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let Ok(mut states) = self.states.lock() {
            if let Some(state) = states.get_mut(&self.key) {
                state.in_flight = false;
            }
        }
    }
}

/// What a background pass will summarize, taken from the request before any
/// summary is applied so message ids still line up with the history.
pub(super) struct Plan {
    pub(super) candidates: Vec<AgentMessage>,
    pub(super) boundary: MessageId,
    pub(super) previous: Option<String>,
    pub(super) task_text: String,
}
