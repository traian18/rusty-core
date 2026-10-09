//! Asking "does this still matter?" about old messages before they are
//! summarized away.
//!
//! [`ImportanceJudge`] is the pluggable seam. Compaction works without one;
//! activating [`JevImportanceJudge`] (the OpenRouter JEV Decisions API) lets
//! the compactor keep essential messages verbatim and skip disposable ones
//! entirely instead of paying to summarize them.

use std::collections::HashMap;

use async_trait::async_trait;
use harness_protocol::ids::MessageId;
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use serde_json::{json, Value};

/// How much a message still matters to the work in progress, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Importance {
    /// Keep verbatim: decisions, constraints, the plan, facts later steps depend on.
    Essential,
    /// Worth a line in the summary.
    Useful,
    /// Safe to drop without mention: dead ends, chatter, superseded output.
    Disposable,
}

impl Importance {
    const ALL: [Importance; 3] = [Self::Essential, Self::Useful, Self::Disposable];

    /// One step towards `Essential`. Dropping something needed costs more than
    /// keeping something that was not, so unsure verdicts lean this way.
    fn step_up(self) -> Self {
        match self {
            Self::Disposable => Self::Useful,
            _ => Self::Essential,
        }
    }
}

/// One message put to the judge, reduced to what a verdict needs.
#[derive(Debug, Clone)]
pub struct JudgeItem {
    pub id: MessageId,
    pub role: MessageRole,
    /// Clipped text of the message.
    pub excerpt: String,
}

impl JudgeItem {
    pub fn from_message(message: &AgentMessage, max_chars: usize) -> Self {
        let mut excerpt = String::new();
        for block in &message.content {
            let piece = match block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolUse { call } => format!("[called {}]", call.name),
                ContentBlock::ToolResult { result, .. } => {
                    format!("[tool result] {}", result.output_preview)
                }
                ContentBlock::Image { .. } => "[image]".to_string(),
            };
            excerpt.push_str(&piece);
            excerpt.push('\n');
        }
        Self {
            id: message.id,
            role: message.role,
            excerpt: clip(&excerpt, max_chars),
        }
    }
}

/// A verdict on one [`JudgeItem`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub id: MessageId,
    pub importance: Importance,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct JudgeError(pub String);

impl std::fmt::Display for JudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JudgeError {}

/// Rates how much each message still matters.
#[async_trait]
pub trait ImportanceJudge: Send + Sync {
    /// `task` is the original request plus recent progress, so verdicts are
    /// relative to what the session is doing now. Items missing from the
    /// result are treated as `Useful`.
    async fn judge(&self, task: &str, items: &[JudgeItem]) -> Result<Vec<Verdict>, JudgeError>;
}

/// Posts a JSON body to the JEV Decisions API and returns the JSON answer.
/// Kept as a trait so tests and hosts can supply their own HTTP stack.
#[async_trait]
pub trait DecisionsTransport: Send + Sync {
    async fn post(&self, body: Value) -> Result<Value, JudgeError>;
}

/// JEV (OpenRouter Decisions API) as an [`ImportanceJudge`].
///
/// One `score` question per message, batched into a single request. As in the
/// model selector, a verdict below `confidence_threshold` is stepped up one
/// level towards `Essential`.
pub struct JevImportanceJudge<T: DecisionsTransport> {
    transport: T,
    model_id: String,
    confidence_threshold: f64,
    batch_size: usize,
}

const INSTRUCTIONS: &str = "How much does this earlier message still matter for finishing the work in progress? Choose the lowest level that is enough: anything needlessly kept as essential wastes context the assistant needs for the rest of the task.";
const CRITERIA: [&str; 3] = [
    "Essential: states a requirement, constraint, decision, plan or fact the remaining work depends on, and cannot be recovered by re-reading files or re-running a command.",
    "Useful: background worth one line in a summary, such as what was tried or what a tool found, but not needed word for word.",
    "Disposable: chatter, a dead end, or output that was superseded or can be trivially regenerated.",
];

impl<T: DecisionsTransport> JevImportanceJudge<T> {
    pub fn new(transport: T, model_id: impl Into<String>) -> Self {
        Self {
            transport,
            model_id: model_id.into(),
            confidence_threshold: 0.6,
            batch_size: 8,
        }
    }

    pub fn with_confidence_threshold(mut self, threshold: f64) -> Self {
        self.confidence_threshold = threshold;
        self
    }

    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    fn request_body(&self, task: &str, batch: &[JudgeItem]) -> Value {
        let state = format!(
            "An AI assistant in a code editor is partway through a long task. Its earlier messages are about to be compressed. Current task and progress:\n{task}\n\nEach question below is about one earlier message."
        );
        let mut questions = serde_json::Map::new();
        for (index, item) in batch.iter().enumerate() {
            questions.insert(
                format!("m{index}"),
                json!({
                    "type": "score",
                    "instructions": format!(
                        "{INSTRUCTIONS}\n\nMessage ({:?}):\n{}",
                        item.role, item.excerpt
                    ),
                    "criteria": CRITERIA,
                }),
            );
        }
        json!({ "model": self.model_id, "state": state, "questions": questions })
    }

    fn parse(&self, batch: &[JudgeItem], response: &Value) -> Vec<Verdict> {
        let mut verdicts = Vec::new();
        for (index, item) in batch.iter().enumerate() {
            let Some(answer) = response.pointer(&format!("/answers/m{index}")) else {
                continue;
            };
            let Some(confidence) = answer.get("confidence").and_then(Value::as_f64) else {
                continue;
            };
            let probabilities = answer.get("probabilities");
            let probability = |position: usize| {
                probabilities
                    .and_then(|p| p.get(position.to_string()))
                    .and_then(Value::as_f64)
            };
            let ranked: HashMap<usize, f64> = (0..Importance::ALL.len())
                .filter_map(|i| probability(i).map(|p| (i, p)))
                .collect();
            // Highest probability wins; ties go to the more important level.
            let Some(top) = (0..Importance::ALL.len())
                .filter(|i| ranked.contains_key(i))
                .max_by(|a, b| {
                    ranked[a]
                        .partial_cmp(&ranked[b])
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(b.cmp(a))
                })
            else {
                continue;
            };
            let mut importance = Importance::ALL[top];
            if confidence < self.confidence_threshold {
                importance = importance.step_up();
            }
            verdicts.push(Verdict {
                id: item.id,
                importance,
                confidence,
            });
        }
        verdicts
    }
}

#[async_trait]
impl<T: DecisionsTransport> ImportanceJudge for JevImportanceJudge<T> {
    async fn judge(&self, task: &str, items: &[JudgeItem]) -> Result<Vec<Verdict>, JudgeError> {
        let mut all = Vec::with_capacity(items.len());
        for batch in items.chunks(self.batch_size) {
            let response = self.transport.post(self.request_body(task, batch)).await?;
            all.extend(self.parse(batch, &response));
        }
        Ok(all)
    }
}

/// `https://openrouter.ai/api/alpha/decisions` over reqwest.
#[cfg(feature = "jev-http")]
pub struct OpenRouterDecisions {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
}

#[cfg(feature = "jev-http")]
impl OpenRouterDecisions {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            endpoint: "https://openrouter.ai/api/alpha/decisions".to_string(),
        }
    }
}

#[cfg(feature = "jev-http")]
#[async_trait]
impl DecisionsTransport for OpenRouterDecisions {
    async fn post(&self, body: Value) -> Result<Value, JudgeError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://rusty.dev")
            .header("X-Title", "Rusty")
            .timeout(std::time::Duration::from_secs(12))
            .json(&body)
            .send()
            .await
            .map_err(|e| JudgeError(format!("JEV unreachable: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(JudgeError(format!("JEV returned {status}")));
        }
        response
            .json()
            .await
            .map_err(|e| JudgeError(format!("JEV returned invalid JSON: {e}")))
    }
}

pub(crate) fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}… [clipped]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::ids::Timestamp;
    use std::sync::Mutex;

    struct Canned(Mutex<Vec<Value>>);

    #[async_trait]
    impl DecisionsTransport for Canned {
        async fn post(&self, body: Value) -> Result<Value, JudgeError> {
            self.0.lock().unwrap().push(body);
            Ok(json!({"answers": {
                "m0": {"type":"score","confidence":0.9,"probabilities":{"0":0.8,"1":0.15,"2":0.05}},
                "m1": {"type":"score","confidence":0.9,"probabilities":{"0":0.05,"1":0.15,"2":0.8}},
                "m2": {"type":"score","confidence":0.3,"probabilities":{"0":0.1,"1":0.2,"2":0.7}},
            }}))
        }
    }

    fn item(text: &str) -> JudgeItem {
        JudgeItem::from_message(
            &AgentMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: text.into() }],
                created_at: Timestamp::now(),
            },
            100,
        )
    }

    #[tokio::test]
    async fn maps_probabilities_and_steps_up_when_unsure() {
        let judge = JevImportanceJudge::new(Canned(Mutex::new(vec![])), "jev-test");
        let items = [item("keep"), item("drop"), item("unsure")];
        let verdicts = judge.judge("task", &items).await.unwrap();
        assert_eq!(verdicts[0].importance, Importance::Essential);
        assert_eq!(verdicts[1].importance, Importance::Disposable);
        // Disposable at 0.3 confidence steps up one level.
        assert_eq!(verdicts[2].importance, Importance::Useful);
        let bodies = judge.transport.0.lock().unwrap();
        assert_eq!(bodies.len(), 1, "one request for the whole batch");
        assert_eq!(bodies[0]["model"], "jev-test");
        assert!(bodies[0]["questions"]["m2"]["instructions"]
            .as_str()
            .unwrap()
            .contains("unsure"));
    }

    #[tokio::test]
    async fn malformed_answers_are_skipped() {
        struct Bad;
        #[async_trait]
        impl DecisionsTransport for Bad {
            async fn post(&self, _: Value) -> Result<Value, JudgeError> {
                Ok(json!({"answers": {"m0": {"type": "score"}}}))
            }
        }
        let verdicts = JevImportanceJudge::new(Bad, "m")
            .judge("t", &[item("x")])
            .await
            .unwrap();
        assert!(verdicts.is_empty());
    }
}
