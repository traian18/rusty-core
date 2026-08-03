use harness_protocol::{
    events::{AgentEvent, AgentEventEnvelope},
    ids::PermissionId,
};

#[derive(Default)]
pub struct AppState {
    pub input: String,
    pub status: String,
    pub events: Vec<String>,
    pub pending_permission: Option<PermissionId>,
    pub should_quit: bool,
}

impl AppState {
    pub fn from_snapshot(status: impl std::fmt::Debug) -> Self {
        Self {
            status: format!("{status:?}"),
            ..Self::default()
        }
    }
    pub fn fold_event(&mut self, envelope: AgentEventEnvelope) {
        if let AgentEvent::PermissionRequested { request } = &envelope.event {
            self.pending_permission = Some(request.id);
        }
        self.events.push(format!("● {:?}", envelope.event));
        if self.events.len() > 200 {
            self.events.remove(0);
        }
    }
}
