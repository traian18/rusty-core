//! One request end to end: admission, the attempt/retry loop, metrics.

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use harness_model::events::ModelError;
use harness_model::request::ModelRequest;
use harness_protocol::backend::{
    ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
};
use harness_protocol::ids::RequestId;

use super::convert::{emit_terminal, to_execution_error, to_execution_result};
use super::retry::{
    append_interrupted_response, classify_attempt, retry_delay, wait_retry_delay, AttemptDecision,
    RetryContext,
};
use super::stream::AttemptProgress;
use super::GenericModelBackend;

impl GenericModelBackend {
    pub(super) async fn execute_request(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let request_id = request.request_id;
        let call_start = tokio::time::Instant::now();
        if let Some(error) = self.check_capabilities(&request) {
            return self.reject_request(&sink, request_id, "rejected_capability", error);
        }

        let model_request = prepare_model_request(request);
        if let Err(error) = self.circuit.allow_request() {
            return self.reject_request(&sink, request_id, "rejected_circuit_open", error);
        }

        let (final_result, attempts) = self
            .execute_attempts(model_request, request_id, &sink, &cancel)
            .await;
        if let Err(ExecutionError::Timeout) = &final_result {
            self.circuit.record_failure(&ModelError::Timeout);
        }
        self.record_request_metrics(&final_result, attempts, call_start);
        emit_terminal(&sink, request_id, &final_result);
        final_result
    }

    async fn execute_attempts(
        &self,
        mut model_request: ModelRequest,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
    ) -> (Result<ExecutionResult, ExecutionError>, usize) {
        let mut attempt = 1;
        loop {
            let mut progress = AttemptProgress::default();
            let decision = self
                .attempt_once(
                    &model_request,
                    request_id,
                    sink,
                    cancel,
                    attempt,
                    &mut progress,
                )
                .await;
            let AttemptDecision::Retry(error) = decision else {
                return (self.settle(decision, request_id), attempt);
            };
            if let Err(error) = self
                .prepare_retry(error, attempt, &mut model_request, progress, cancel)
                .await
            {
                return (Err(error), attempt);
            }
            attempt += 1;
        }
    }

    async fn attempt_once(
        &self,
        model_request: &ModelRequest,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
        attempt: usize,
        progress: &mut AttemptProgress,
    ) -> AttemptDecision {
        let outcome = self
            .run_attempt(model_request.clone(), request_id, sink, cancel, progress)
            .await;
        let retry_context = RetryContext {
            emitted_output: outcome.emitted_output,
            progress,
            response_format: &model_request.response_format,
            attempt,
            max_attempts: self.recovery.max_attempts,
        };
        classify_attempt(outcome.result, cancel.is_cancelled(), &retry_context)
    }

    /// Final result for any decision that ends the loop. A `Retry` only gets
    /// here if a caller skipped `prepare_retry`, so it counts as a failure.
    fn settle(
        &self,
        decision: AttemptDecision,
        request_id: RequestId,
    ) -> Result<ExecutionResult, ExecutionError> {
        match decision {
            AttemptDecision::Completed(result) => {
                self.circuit.record_success();
                Ok(to_execution_result(request_id, result))
            }
            AttemptDecision::Cancelled => Err(ExecutionError::Cancelled),
            AttemptDecision::Failed(error) | AttemptDecision::Retry(error) => {
                self.circuit.record_failure(&error);
                Err(to_execution_error(error))
            }
        }
    }

    /// Waits out the backoff and reshapes the request for the next attempt.
    /// `Err` ends the loop with that result.
    async fn prepare_retry(
        &self,
        error: ModelError,
        attempt: usize,
        model_request: &mut ModelRequest,
        progress: AttemptProgress,
        cancel: &CancellationToken,
    ) -> Result<(), ExecutionError> {
        let delay = retry_delay(attempt, &error);
        if delay >= self.recovery.idle_timeout {
            self.circuit.record_failure(&error);
            return Err(to_execution_error(error));
        }
        if !progress.text.is_empty() {
            append_interrupted_response(model_request, progress.text);
        }
        warn!(attempt, ?delay, error = %error, "retrying transient model provider failure");
        if wait_retry_delay(delay, cancel).await {
            Ok(())
        } else {
            Err(ExecutionError::Cancelled)
        }
    }

    fn reject_request(
        &self,
        sink: &broadcast::Sender<ExecutionEvent>,
        request_id: RequestId,
        outcome: &'static str,
        error: ModelError,
    ) -> Result<ExecutionResult, ExecutionError> {
        metrics::counter!("harness_backend_requests_total", "backend" => self.descriptor.name.clone(), "outcome" => outcome).increment(1);
        let result = Err(to_execution_error(error));
        emit_terminal(sink, request_id, &result);
        result
    }

    fn record_request_metrics(
        &self,
        result: &Result<ExecutionResult, ExecutionError>,
        attempts: usize,
        start: tokio::time::Instant,
    ) {
        let backend = self.descriptor.name.clone();
        metrics::histogram!("harness_backend_request_duration_seconds", "backend" => backend.clone())
            .record(start.elapsed().as_secs_f64());
        metrics::counter!(
            "harness_backend_requests_total",
            "backend" => backend.clone(),
            "outcome" => if result.is_ok() { "success" } else { "error" }
        )
        .increment(1);
        metrics::counter!("harness_backend_request_attempts_total", "backend" => backend.clone())
            .increment(attempts as u64);
        metrics::gauge!("harness_backend_circuit_open", "backend" => backend)
            .set(if self.circuit.is_open() { 1.0 } else { 0.0 });
    }
}

fn prepare_model_request(request: ExecutionRequest) -> ModelRequest {
    ModelRequest {
        system_prompt: request.system_prompt,
        messages: request.messages,
        tools: request.tools,
        model: request.params.model,
        max_tokens: request.params.max_tokens,
        temperature: request.params.temperature,
        stop_sequences: request.params.stop_sequences,
        extended_thinking: request.extended_thinking,
        reasoning_effort: request.params.reasoning_effort,
        response_format: request.params.response_format,
        provider_options: request.params.provider_options,
        cache_key: Some(request.run_id.to_string()),
    }
}
