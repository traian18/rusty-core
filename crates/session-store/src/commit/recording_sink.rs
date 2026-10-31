//! An in-memory record of published events.

use std::sync::Mutex;

pub struct RecordingSink {
    published: Mutex<Vec<rusty_protocol::events::AgentEventEnvelope>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self {
            published: Mutex::new(Vec::new()),
        }
    }

    pub fn record(&self, envelope: rusty_protocol::events::AgentEventEnvelope) {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(envelope);
    }

    pub fn published(&self) -> Vec<rusty_protocol::events::AgentEventEnvelope> {
        self.published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn sequences(&self) -> Vec<u64> {
        self.published()
            .into_iter()
            .filter_map(|envelope| envelope.session_sequence)
            .collect()
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}
