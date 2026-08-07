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

#[cfg(test)]
mod tests {
    use super::*;

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
