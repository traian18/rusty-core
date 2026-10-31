//! Provider-agnostic transcript message types.

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, Timestamp, ToolCallId};
pub use crate::tools::{ToolCall, ToolResultSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Serializes `Vec<u8>` as a base64 string in JSON (matching the wire shape
/// every image-capable provider API already expects for inline image data)
/// instead of serde_json's default JSON-array-of-numbers, which would both
/// bloat durable storage ~4x and need re-encoding at every provider
/// boundary anyway.
mod base64_bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        call: ToolCall,
    },
    ToolResult {
        call_id: ToolCallId,
        result: ToolResultSummary,
    },
    /// Inline image content. `data` is the raw (non-base64-encoded) image
    /// bytes in memory; base64 encoding happens only at JSON-serialization
    /// boundaries (durable storage, provider wire formats) via
    /// `base64_bytes`.
    Image {
        mime_type: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageId,
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub created_at: Timestamp,
}

/// Repair the provider-facing copy of history after truncation or interruption.
/// Move existing results beside their calls; missing outcomes are explicitly
/// unknown, never successful. Orphaned results remain readable context.
pub fn repair_tool_history(messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
    use std::collections::{HashMap, HashSet};
    let mut results = HashMap::new();
    let calls: HashSet<_> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| {
            if let ContentBlock::ToolUse { call } = b {
                Some(call.id)
            } else {
                None
            }
        })
        .collect();
    for block in messages.iter().flat_map(|m| &m.content) {
        if let ContentBlock::ToolResult { call_id, result } = block {
            results.entry(*call_id).or_insert_with(|| result.clone());
        }
    }
    let mut repaired = Vec::new();
    for mut message in messages {
        let mut replies = Vec::new();
        message.content = message.content.into_iter().filter_map(|block| match block {
            ContentBlock::ToolResult { call_id, result } => {
                (!calls.contains(&call_id)).then(|| ContentBlock::Text {
                    text: format!("Earlier tool result ({call_id}; call omitted from context): {}", result.output_preview),
                })
            }
            ContentBlock::ToolUse { ref call } => {
                replies.push(ContentBlock::ToolResult {
                    call_id: call.id,
                    result: results.remove(&call.id).unwrap_or_else(|| ToolResultSummary {
                        has_error: true,
                        output_preview: "Tool outcome unavailable: execution may have been interrupted. Inspect the current state before retrying any action; do not assume it failed or succeeded.".into(),
                    }),
                });
                Some(block)
            }
            _ => Some(block),
        }).collect();
        if message.role == MessageRole::Tool {
            message.role = MessageRole::User;
        }
        if !message.content.is_empty() {
            repaired.push(message.clone());
        }
        if !replies.is_empty() {
            repaired.push(AgentMessage {
                id: message.id,
                role: MessageRole::Tool,
                content: replies,
                created_at: message.created_at,
            });
        }
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: MessageRole, content: Vec<ContentBlock>) -> AgentMessage {
        AgentMessage {
            id: MessageId::new(),
            role,
            content,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn repair_preserves_real_results_and_fills_only_missing_outcomes() {
        let first = ToolCallId::new();
        let missing = ToolCallId::new();
        let orphan = ToolCallId::new();
        let call = |id| ContentBlock::ToolUse {
            call: ToolCall {
                id,
                name: "write_file".into(),
                arguments: serde_json::json!({}),
            },
        };
        let result = |call_id, text: &str| ContentBlock::ToolResult {
            call_id,
            result: ToolResultSummary {
                has_error: false,
                output_preview: text.into(),
            },
        };
        let repaired = repair_tool_history(vec![
            message(MessageRole::Assistant, vec![call(first), call(missing)]),
            message(
                MessageRole::Assistant,
                vec![ContentBlock::Text {
                    text: "progress".into(),
                }],
            ),
            message(
                MessageRole::Tool,
                vec![result(first, "written successfully")],
            ),
            message(MessageRole::Tool, vec![result(orphan, "earlier read")]),
        ]);
        assert_eq!(repaired[1].role, MessageRole::Tool);
        assert!(
            matches!(&repaired[1].content[0], ContentBlock::ToolResult { call_id, result } if *call_id == first && result.output_preview == "written successfully" && !result.has_error)
        );
        assert!(
            matches!(&repaired[1].content[1], ContentBlock::ToolResult { call_id, result } if *call_id == missing && result.has_error && result.output_preview.contains("Inspect the current state"))
        );
        assert_eq!(repaired[3].role, MessageRole::User);
        assert!(
            matches!(&repaired[3].content[0], ContentBlock::Text { text } if text.contains("earlier read"))
        );
        assert_eq!(
            serde_json::to_value(&repaired).unwrap(),
            serde_json::to_value(repair_tool_history(repaired.clone())).unwrap(),
            "repair is idempotent"
        );
    }

    #[test]
    fn repair_keeps_valid_parallel_call_history_unchanged() {
        let id = ToolCallId::new();
        let messages = vec![
            message(
                MessageRole::Assistant,
                vec![ContentBlock::ToolUse {
                    call: ToolCall {
                        id,
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    },
                }],
            ),
            message(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult {
                    call_id: id,
                    result: ToolResultSummary {
                        has_error: false,
                        output_preview: "file".into(),
                    },
                }],
            ),
        ];
        let repaired = repair_tool_history(messages.clone());
        assert_eq!(
            serde_json::to_value(&messages[0].content).unwrap(),
            serde_json::to_value(&repaired[0].content).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&messages[1].content).unwrap(),
            serde_json::to_value(&repaired[1].content).unwrap()
        );
    }

    #[test]
    fn message_round_trips_through_json() {
        let message = AgentMessage {
            id: MessageId::new(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                call: ToolCall {
                    id: ToolCallId::new(),
                    name: "search".into(),
                    arguments: serde_json::json!({"query": "rust"}),
                },
            }],
            created_at: Timestamp::now(),
        };

        let json = serde_json::to_string(&message).expect("serialize message");
        let decoded: AgentMessage = serde_json::from_str(&json).expect("deserialize message");
        assert_eq!(decoded.role, MessageRole::Assistant);
        assert!(matches!(decoded.content[0], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn image_content_block_round_trips_as_base64_json() {
        let block = ContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF],
        };
        let json = serde_json::to_value(&block).expect("serialize image block");
        // The `data` field must be a JSON string (base64), not an array of numbers.
        let data_value = json
            .get("Image")
            .and_then(|v| v.get("data"))
            .expect("data field present");
        assert!(
            data_value.is_string(),
            "image bytes must serialize as a base64 string, got {data_value:?}"
        );

        let decoded: ContentBlock = serde_json::from_value(json).expect("deserialize image block");
        match decoded {
            ContentBlock::Image { mime_type, data } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(data, vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]);
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }
}
