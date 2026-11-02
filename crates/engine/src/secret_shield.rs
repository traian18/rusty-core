//! Local outbound credential shielding for model requests.
//!
//! This component only detects credential-shaped strings. It does not inspect
//! images and cannot guarantee detection of every secret or personal datum.

use std::sync::Arc;

use async_trait::async_trait;
use regex::{Captures, Regex};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use rusty_protocol::backend::{ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult};
use rusty_protocol::messages::{AgentMessage, ContentBlock};
use rusty_runtime::traits::ExecutionBackend;
use serde_json::Value;

const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretShieldPolicy {
    Off,
    Warn,
    #[default]
    Mask,
    Block,
}

#[derive(Debug)]
pub struct SecretShield {
    patterns: Vec<Regex>,
}

impl Default for SecretShield {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretShield {
    pub fn new() -> Self {
        let patterns = [
            r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})\b",
            r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b",
            r#"(?i)(?P<prefix>\b(?:password|passwd|pwd|token|api[_-]?key|secret)[\"']?\s*[=:]\s*[\"']?)[^\s,;\"']{8,}"#,
            r"(?i)(?P<prefix>authorization\s*:\s*(?:bearer|basic)\s+)[A-Za-z0-9._~+/-]{8,}={0,2}",
            r"(?i)(?P<prefix>://[^:/@\s]+:)[^@/\s]{8,}@",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("static secret pattern is valid"))
        .collect();
        Self { patterns }
    }

    pub fn redact(&self, text: &str) -> (String, usize) {
        let mut redacted = text.to_owned();
        let mut detections = 0;
        for pattern in &self.patterns {
            redacted = pattern
                .replace_all(&redacted, |captures: &Captures<'_>| {
                    detections += 1;
                    if let Some(prefix) = captures.name("prefix") {
                        let suffix = if captures[0].ends_with('@') { "@" } else { "" };
                        format!("{}{REDACTED}{suffix}", prefix.as_str())
                    } else {
                        REDACTED.to_owned()
                    }
                })
                .into_owned();
        }
        (redacted, detections)
    }

    pub fn rewrite_request(&self, request: &mut ExecutionRequest) -> usize {
        let mut detections = 0;
        detections += self.rewrite_text(&mut request.system_prompt);
        for message in &mut request.messages {
            detections += self.rewrite_message(message);
        }
        for tool in &mut request.tools {
            detections += self.rewrite_text(&mut tool.description);
            detections += self.rewrite_value(&mut tool.input_schema);
        }
        detections
    }

    fn rewrite_message(&self, message: &mut AgentMessage) -> usize {
        message
            .content
            .iter_mut()
            .map(|block| match block {
                ContentBlock::Text { text } => self.rewrite_text(text),
                ContentBlock::ToolUse { call } => self.rewrite_value(&mut call.arguments),
                ContentBlock::ToolResult { result, .. } => {
                    self.rewrite_text(&mut result.output_preview)
                }
                ContentBlock::Image { .. } => 0,
            })
            .sum()
    }

    fn rewrite_text(&self, text: &mut String) -> usize {
        let (redacted, detections) = self.redact(text);
        *text = redacted;
        detections
    }

    fn rewrite_value(&self, value: &mut Value) -> usize {
        match value {
            Value::String(text) => self.rewrite_text(text),
            Value::Array(items) => items.iter_mut().map(|item| self.rewrite_value(item)).sum(),
            Value::Object(fields) => fields
                .iter_mut()
                .map(|(key, item)| {
                    let sensitive = matches!(
                        key.to_ascii_lowercase().as_str(),
                        "password"
                            | "passwd"
                            | "pwd"
                            | "token"
                            | "api_key"
                            | "api-key"
                            | "apikey"
                            | "secret"
                    );
                    if sensitive {
                        if let Value::String(text) = item {
                            if text.len() >= 8 && text != REDACTED {
                                *text = REDACTED.to_owned();
                                return 1;
                            }
                        }
                    }
                    self.rewrite_value(item)
                })
                .sum(),
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        }
    }
}

pub struct SecretShieldBackend {
    inner: Arc<dyn ExecutionBackend>,
    policy: SecretShieldPolicy,
    shield: SecretShield,
}

impl SecretShieldBackend {
    pub fn new(inner: Arc<dyn ExecutionBackend>, policy: SecretShieldPolicy) -> Self {
        Self {
            inner,
            policy,
            shield: SecretShield::new(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for SecretShieldBackend {
    fn descriptor(&self) -> rusty_protocol::backend::BackendDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> rusty_protocol::backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn execute(
        &self,
        mut request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        if self.policy == SecretShieldPolicy::Off {
            return self.inner.execute(request, sink, cancel).await;
        }

        let original_request = request.clone();
        let detections = self.shield.rewrite_request(&mut request);
        if detections == 0 {
            return self.inner.execute(request, sink, cancel).await;
        }

        match self.policy {
            SecretShieldPolicy::Off => unreachable!("handled above"),
            SecretShieldPolicy::Warn => {
                tracing::warn!(
                    request_id = %request.request_id,
                    detections,
                    "Secret Shield detected credential-shaped content; sending unchanged request"
                );
                self.inner.execute(original_request, sink, cancel).await
            }
            SecretShieldPolicy::Mask => self.inner.execute(request, sink, cancel).await,
            SecretShieldPolicy::Block => Err(ExecutionError::BackendError {
                message: format!(
                    "Secret Shield blocked request ({detections} credential match(es))"
                ),
                code: "secret_shield_blocked".into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_protocol::backend::{BackendCapabilities, BackendDescriptor, ExecutionParams};
    use rusty_protocol::ids::{BackendId, MessageId, RequestId, RunId};
    use rusty_protocol::messages::MessageRole;
    use rusty_protocol::tools::ToolDescriptor;
    use std::sync::Mutex;

    struct RecordingBackend(Mutex<Vec<ExecutionRequest>>);

    #[async_trait]
    impl ExecutionBackend for RecordingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: BackendId::new(),
                name: "recording".into(),
                description: String::new(),
                capabilities: BackendCapabilities::default(),
            }
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn execute(
            &self,
            request: ExecutionRequest,
            _sink: broadcast::Sender<ExecutionEvent>,
            _cancel: CancellationToken,
        ) -> Result<ExecutionResult, ExecutionError> {
            self.0.lock().unwrap().push(request.clone());
            Ok(ExecutionResult {
                request_id: request.request_id,
                usage: Default::default(),
                cost: Default::default(),
                finish_reason: "test".into(),
            })
        }
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: "api_key=sk-12345678901234567890".into(),
            messages: vec![AgentMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: vec![
                    ContentBlock::Text {
                        text: "token: ghp_abcdefghijklmnopqrstuvwxyz123456".into(),
                    },
                    ContentBlock::ToolUse {
                        call: rusty_protocol::tools::ToolCall {
                            id: rusty_protocol::ids::ToolCallId::new(),
                            name: "tool".into(),
                            arguments: serde_json::json!({"nested": ["password=hunter2secret"]}),
                        },
                    },
                    ContentBlock::ToolResult {
                        call_id: rusty_protocol::ids::ToolCallId::new(),
                        result: rusty_protocol::tools::ToolResultSummary {
                            output_preview: "Authorization: Bearer abcdefghijklmnop".into(),
                            has_error: false,
                        },
                    },
                    ContentBlock::Image {
                        mime_type: "image/png".into(),
                        data: vec![1, 2, 3],
                    },
                ],
                created_at: rusty_protocol::ids::Timestamp::now(),
            }],
            tools: vec![ToolDescriptor {
                id: rusty_protocol::ids::ToolId::new(),
                name: "search".into(),
                description: "use token=abcdefghijklmnop".into(),
                input_schema: serde_json::json!({"description": "secret=abcdefghijk"}),
            }],
            extended_thinking: false,
            params: ExecutionParams::default(),
        }
    }

    #[tokio::test]
    async fn mask_redacts_text_tools_and_results_but_not_images() {
        let inner = Arc::new(RecordingBackend(Mutex::new(Vec::new())));
        let shield = SecretShieldBackend::new(inner.clone(), SecretShieldPolicy::Mask);
        let original = request();
        let request_id = original.request_id;
        let expected_image = original.messages[0].content[3].clone();
        let (events, _) = broadcast::channel(1);
        shield
            .execute(original, events, CancellationToken::new())
            .await
            .unwrap();
        let seen = inner.0.lock().unwrap();
        let forwarded = &seen[0];
        assert_eq!(forwarded.request_id, request_id);
        assert!(forwarded.system_prompt.contains(REDACTED));
        let serialized = serde_json::to_value(forwarded).unwrap();
        let original_image = serde_json::to_value(expected_image).unwrap();
        assert!(serialized["messages"][0]["content"][0]["Text"]["text"]
            .as_str()
            .unwrap()
            .contains(REDACTED));
        assert!(
            serialized["messages"][0]["content"][1]["ToolUse"]["call"]["arguments"]["nested"][0]
                .as_str()
                .unwrap()
                .contains(REDACTED)
        );
        assert!(
            serialized["messages"][0]["content"][2]["ToolResult"]["result"]["output_preview"]
                .as_str()
                .unwrap()
                .contains(REDACTED)
        );
        assert_eq!(serialized["messages"][0]["content"][3], original_image);
        assert!(forwarded.tools[0].description.contains(REDACTED));
        assert!(forwarded.tools[0].input_schema["description"]
            .as_str()
            .unwrap()
            .contains(REDACTED));
    }

    #[tokio::test]
    async fn warn_forwards_original_and_block_never_delegates() {
        let inner = Arc::new(RecordingBackend(Mutex::new(Vec::new())));
        let warn = SecretShieldBackend::new(inner.clone(), SecretShieldPolicy::Warn);
        let original = request();
        let expected_prompt = original.system_prompt.clone();
        let (events, _) = broadcast::channel(1);
        warn.execute(original, events, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(inner.0.lock().unwrap()[0].system_prompt, expected_prompt);

        let before = inner.0.lock().unwrap().len();
        let block = SecretShieldBackend::new(inner.clone(), SecretShieldPolicy::Block);
        let (events, _) = broadcast::channel(1);
        let error = block
            .execute(request(), events, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutionError::BackendError { .. }));
        assert_eq!(inner.0.lock().unwrap().len(), before);
        assert!(!format!("{error:?}").contains("sk-12345678901234567890"));
    }

    #[test]
    fn credential_detector_preserves_safe_text_and_redacts_repeated_values() {
        let shield = SecretShield::new();
        let (safe, count) = shield.redact("ordinary text and user@example.com");
        assert_eq!(safe, "ordinary text and user@example.com");
        assert_eq!(count, 0);
        let (redacted, count) = shield.redact("token=abcdefghijk token=abcdefghijk");
        assert_eq!(count, 2);
        assert_eq!(redacted, "token=[REDACTED] token=[REDACTED]");
    }
}
