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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { call: ToolCall },
    ToolResult {
        call_id: ToolCallId,
        result: ToolResultSummary,
    },
    Image { placeholder: serde_json::Value },
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
}
