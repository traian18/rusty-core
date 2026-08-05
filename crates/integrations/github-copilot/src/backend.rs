use std::{process::Stdio, sync::Arc};

use async_trait::async_trait;
use harness_protocol::{
    backend::{BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult},
    ids::BackendId,
    messages::{ContentBlock, MessageRole},
    usage::{Cost, ModelUsage},
};
use harness_runtime::{traits::ExecutionBackend, IntegrationFactory};
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{config::GitHubCopilotConfig, wire::parse_output};

pub struct GitHubCopilotBackend { config: GitHubCopilotConfig }

impl GitHubCopilotBackend {
    pub fn new(config: GitHubCopilotConfig) -> Self { Self { config } }
}

fn latest_user_text(request: &ExecutionRequest) -> Option<String> {
    let message = request.messages.iter().rev().find(|message| message.role == MessageRole::User)?;
    let text = message.content.iter().filter_map(|block| match block {
        ContentBlock::Text { text } => Some(text.as_str()), _ => None,
    }).collect::<Vec<_>>().join("");
    (!text.is_empty()).then_some(text)
}

#[async_trait]
impl ExecutionBackend for GitHubCopilotBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "GitHub Copilot".into(),
            description: "GitHub Copilot CLI programmatic JSON mode".into(),
            capabilities: self.capabilities(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            streaming: false,
            backend_managed_tools: true,
            host_managed_tools: false,
            resumable_sessions: false,
            ..Default::default()
        }
    }

    async fn execute(&self, request: ExecutionRequest, sink: broadcast::Sender<ExecutionEvent>, cancel: CancellationToken) -> Result<ExecutionResult, ExecutionError> {
        let prompt = latest_user_text(&request).ok_or_else(|| ExecutionError::InvalidRequest { message: "no user message to send to Copilot".into() })?;
        let mut command = tokio::process::Command::new(&self.config.binary_path);
        command.arg("--prompt").arg(prompt).arg("--output-format=json").arg("--allow-all-tools").arg("--model").arg(&self.config.model);
        if let Some(directory) = &self.config.working_dir { command.current_dir(directory); }
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| ExecutionError::BackendError { message: format!("failed to spawn Copilot CLI: {error}"), code: "spawn_failed".into() })?;
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let mut output = Vec::new();
        let mut error_output = Vec::new();
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(ExecutionError::Cancelled);
            }
            result = async {
                tokio::try_join!(stdout.read_to_end(&mut output), stderr.read_to_end(&mut error_output))
            } => { result.map_err(|error| ExecutionError::BackendError { message: format!("failed to read Copilot CLI output: {error}"), code: "io_error".into() })?; }
        }
        let status = child.wait().await.map_err(|error| ExecutionError::BackendError { message: format!("Copilot CLI process error: {error}"), code: "wait_failed".into() })?;
        let text = parse_output(&output).map_err(|message| ExecutionError::BackendError {
            message: if error_output.is_empty() { format!("{message} (exit status: {status})") } else { "Copilot CLI reported an error; run `copilot login` and retry".into() },
            code: "invalid_output".into(),
        })?;
        let _ = sink.send(ExecutionEvent::TextDelta { request_id: request.request_id, delta: text });
        let result = ExecutionResult { request_id: request.request_id, usage: ModelUsage::default(), cost: Cost::default(), finish_reason: "completed".into() };
        let _ = sink.send(ExecutionEvent::Completed { request_id: request.request_id, result: result.clone() });
        Ok(result)
    }
}

pub struct GitHubCopilotFactory;

#[async_trait]
impl IntegrationFactory for GitHubCopilotFactory {
    fn id(&self) -> &'static str { "github-copilot" }
    fn descriptor(&self) -> BackendDescriptor { GitHubCopilotBackend::new(GitHubCopilotConfig::default()).descriptor() }
    async fn create(&self, config: serde_json::Value) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Arc::new(GitHubCopilotBackend::new(serde_json::from_value(config)?)))
    }
}
