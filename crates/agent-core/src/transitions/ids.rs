//! Handing out the next sequence number, id and timestamp.

use rusty_protocol::ids::{MessageId, PermissionId, RequestId, RunId, Timestamp};

use crate::agent::Agent;

impl Agent {
    pub(super) fn take_sequence(&mut self) -> u64 {
        let current = self.state.transition_sequence;
        self.state.transition_sequence = current.saturating_add(1);
        current
    }

    pub(super) fn next_run_id(&mut self) -> RunId {
        RunId::derived(self.id.as_uuid(), self.take_sequence(), 1)
    }

    pub(super) fn next_request_id(&mut self) -> RequestId {
        RequestId::derived(self.id.as_uuid(), self.take_sequence(), 2)
    }

    pub(super) fn next_message_id(&mut self) -> MessageId {
        MessageId::derived(self.id.as_uuid(), self.take_sequence(), 3)
    }

    pub(super) fn next_permission_id(&mut self) -> PermissionId {
        PermissionId::derived(self.id.as_uuid(), self.take_sequence(), 4)
    }

    pub(super) fn next_timestamp(&mut self) -> Timestamp {
        Timestamp::from_sequence(self.take_sequence())
    }
}
