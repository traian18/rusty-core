//! Starting a run, or queueing the input while one is in progress.

use rusty_protocol::commands::{AgentStatus, Attachment, UserInput};
use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

use crate::agent::Agent;
use crate::behavior::RuleEvent;
use crate::transcript::validate_transcript;

/// M4: caps a single attachment's raw byte size, mirroring the M3 precedent
/// set by `fs.read`'s 10MB cap. Unlike text truncation, silently truncating
/// image bytes would just produce a corrupt, undecodable image — so an
/// oversized attachment fails the run outright (via `Agent::fail`, the same
/// path `validate_transcript` already uses) rather than being silently
/// mangled or unboundedly accepted.
pub(super) const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

pub(super) fn validate_attachments(attachments: &[Attachment]) -> Result<(), String> {
    for (index, attachment) in attachments.iter().enumerate() {
        if attachment.data.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment {index} ({} bytes, {}) exceeds the {MAX_ATTACHMENT_BYTES}-byte limit",
                attachment.data.len(),
                attachment.mime_type
            ));
        }
    }
    Ok(())
}

/// Converts one user-supplied [`Attachment`] into a transcript
/// [`ContentBlock`]. Image MIME types become a real `ContentBlock::Image`
/// (forwarded to providers that support it — see
/// `GenericModelBackend::check_capabilities`, which rejects an image-bearing
/// request against a provider that doesn't). Anything else becomes a
/// visible text note rather than being silently dropped: this harness does
/// not yet have a wire format for non-image attachments (e.g. arbitrary
/// files), and the M4 requirement is "no silent loss," not "every
/// attachment kind is fully supported."
pub(super) fn attachment_to_content_block(attachment: Attachment) -> ContentBlock {
    if attachment.mime_type.starts_with("image/") {
        ContentBlock::Image {
            mime_type: attachment.mime_type,
            data: attachment.data,
        }
    } else {
        ContentBlock::Text {
            text: format!(
                "[attachment: {} bytes, {} — non-image attachments are not yet forwarded as model input]",
                attachment.data.len(),
                attachment.mime_type
            ),
        }
    }
}

impl Agent {
    pub(super) fn start_or_queue(&mut self, input: UserInput) -> Vec<AgentEffect> {
        if self.state.active_run.is_some() {
            self.state.queued_inputs.push_back(input);
            return Vec::new();
        }
        self.start_run(input)
    }

    pub(super) fn start_run(&mut self, input: UserInput) -> Vec<AgentEffect> {
        if let Err(error) = validate_transcript(&self.state.messages) {
            return self.fail("INVALID_TRANSCRIPT", error.to_string());
        }
        if let Err(error) = validate_attachments(&input.attachments) {
            return self.fail("ATTACHMENT_TOO_LARGE", error);
        }
        let mut effects = Vec::new();
        // A switch requested during the previous run applies to this one
        // from its start, so `RunStart` rules run under it too.
        if let Some((profile, library)) = self.state.pending_profile.take() {
            effects.extend(self.apply_switch(profile, library));
        }
        self.state.behavior.reset_run();
        let from = self.state.status;
        let run_id = self.next_run_id();
        let message_id = self.next_message_id();
        let created_at = self.next_timestamp();
        let mut content = vec![ContentBlock::Text { text: input.text }];
        content.extend(
            input
                .attachments
                .into_iter()
                .map(attachment_to_content_block),
        );
        self.state.messages.push(AgentMessage {
            id: message_id,
            role: MessageRole::User,
            content,
            created_at,
        });
        self.state.status = AgentStatus::PreparingContext;
        self.state.active_run = Some(run_id);
        let outcome = self.state.behavior.evaluate(RuleEvent::RunStart, None);
        effects.extend(self.apply_rule_outcome(&outcome, None));
        if let Some(stop) = &outcome.stop {
            effects.extend(self.behavior_stop(stop));
            return effects;
        }
        match self.begin_turn() {
            Ok(turn_effects) => effects.extend(turn_effects),
            Err(failure) => {
                effects.extend(failure);
                return effects;
            }
        }
        let request = self.execution_request(run_id);
        effects.push(Self::state_changed(from, AgentStatus::PreparingContext));
        effects.push(AgentEffect::ExecuteBackend { request });
        effects.push(AgentEffect::Emit {
            event: AgentEvent::RunStarted { run_id },
        });
        effects
    }

    pub(super) fn start_next_queued_run(&mut self) -> Vec<AgentEffect> {
        self.state
            .queued_inputs
            .pop_front()
            .map(|input| self.start_run(input))
            .unwrap_or_default()
    }
}
