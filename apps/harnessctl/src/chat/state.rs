use harness_protocol::{
    events::{AgentEvent, AgentEventEnvelope},
    ids::{PermissionId, SessionId},
};

/// Mirrors `apps/harness/src/app_state.rs`'s `AppState` — same shape, same
/// event-folding behavior — just fed from pushed RPC `Event` frames instead
/// of a local in-process `broadcast::Receiver`.
pub struct ChatState {
    pub session_id: SessionId,
    pub input: String,
    pub status: String,
    pub log: Vec<String>,
    pub pending_permission: Option<PermissionId>,
    pub should_quit: bool,
}

impl ChatState {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            input: String::new(),
            status: "Idle".to_string(),
            log: Vec::new(),
            pending_permission: None,
            should_quit: false,
        }
    }

    pub fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    pub fn fold_event(&mut self, envelope: AgentEventEnvelope) {
        if let AgentEvent::PermissionRequested { request } = &envelope.event {
            self.pending_permission = Some(request.id);
        }
        if let AgentEvent::StateChanged { to, .. } = &envelope.event {
            self.status = format!("{to:?}");
        }
        self.push_log(format!("● {:?}", envelope.event));
    }
}
