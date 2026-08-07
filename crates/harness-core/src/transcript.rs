//! Validation for the assistant tool-call → tool-result transcript invariant.

use std::collections::HashMap;

use harness_protocol::ids::ToolCallId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptError {
    #[error("Dangling unresolved tool call: {0}")]
    DanglingToolCall(ToolCallId),
    #[error("Tool result {0} without preceding tool call")]
    OrphanToolResult(ToolCallId),
    #[error("Tool result {0} appears before the assistant message that made the call")]
    OutOfOrderToolResult(ToolCallId),
    #[error("Tool call {0} has no corresponding result")]
    UnresolvedToolCall(ToolCallId),
}

pub fn validate_transcript(messages: &[AgentMessage]) -> Result<(), TranscriptError> {
    let declarations: HashMap<ToolCallId, usize> = messages
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            message.content.iter().filter_map(move |block| match block {
                ContentBlock::ToolUse { call } => Some((call.id, index)),
                _ => None,
            })
        })
        .collect();
    let mut pending: Vec<ToolCallId> = Vec::new();

    for (index, message) in messages.iter().enumerate() {
        match message.role {
            MessageRole::Assistant => {
                pending.extend(message.content.iter().filter_map(|block| match block {
                    ContentBlock::ToolUse { call } => Some(call.id),
                    _ => None,
                }));
            }
            MessageRole::Tool => {
                for call_id in message.content.iter().filter_map(|block| match block {
                    ContentBlock::ToolResult { call_id, .. } => Some(*call_id),
                    _ => None,
                }) {
                    if let Some(position) = pending.iter().position(|id| *id == call_id) {
                        pending.remove(position);
                    } else if declarations
                        .get(&call_id)
                        .is_some_and(|declared| *declared > index)
                    {
                        return Err(TranscriptError::OutOfOrderToolResult(call_id));
                    } else {
                        return Err(TranscriptError::OrphanToolResult(call_id));
                    }
                }
            }
            MessageRole::User | MessageRole::System if !pending.is_empty() => {
                return Err(TranscriptError::DanglingToolCall(pending[0]));
            }
            MessageRole::User | MessageRole::System => {}
        }
    }

    pending
        .first()
        .copied()
        .map_or(Ok(()), |id| Err(TranscriptError::UnresolvedToolCall(id)))
}

#[cfg(test)]
mod tests {
    use harness_protocol::ids::{MessageId, Timestamp};
    use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
    use harness_protocol::tools::{ToolCall, ToolResultSummary};

    use super::*;

    fn call_message(call_id: ToolCallId) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                call: ToolCall {
                    id: call_id,
                    name: "test".into(),
                    arguments: serde_json::json!({}),
                },
            }],
            created_at: Timestamp::now(),
        }
    }

    fn result_message(call_id: ToolCallId) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id,
                result: ToolResultSummary {
                    has_error: false,
                    output_preview: "ok".into(),
                },
            }],
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn valid_transcript_passes() {
        let id = ToolCallId::new();
        assert!(validate_transcript(&[call_message(id), result_message(id)]).is_ok());
    }

    #[test]
    fn dangling_call_names_the_id() {
        let id = ToolCallId::new();
        assert_eq!(
            validate_transcript(&[call_message(id)]),
            Err(TranscriptError::UnresolvedToolCall(id))
        );
    }

    #[test]
    fn result_before_call_is_out_of_order() {
        let id = ToolCallId::new();
        assert_eq!(
            validate_transcript(&[result_message(id), call_message(id)]),
            Err(TranscriptError::OutOfOrderToolResult(id))
        );
    }
}
