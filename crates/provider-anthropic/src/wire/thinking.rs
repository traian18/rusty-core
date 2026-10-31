//! Choosing the thinking mode a model accepts.

use rusty_model_backend::events::ModelError;

use super::request::{AnthropicOutputConfig, AnthropicThinking};

/// Whether `model` is known to reject `thinking.type: "enabled"` and accept
/// only `"adaptive"` (Claude Opus 4.7 onward and the Claude 5 family).
/// Matches both Anthropic's own ids (`claude-opus-4-7`) and gateway spellings
/// such as GitHub Copilot's (`claude-opus-4.7`). A model this misses is still
/// handled: the client recognizes the provider's rejection and retries in
/// adaptive mode -- see `is_adaptive_thinking_rejection`.
pub fn requires_adaptive_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase().replace('.', "-");
    if model.contains("mythos") {
        return true;
    }
    ["opus", "sonnet", "haiku", "fable"].iter().any(|family| {
        let Some(rest) = model.split(&format!("{family}-")).nth(1) else {
            return false;
        };
        let mut numbers = rest.split('-').map(|part| part.parse::<u32>().ok()).take(2);
        let major = numbers.next().flatten();
        let minor = numbers.next().flatten().filter(|minor| *minor < 100);
        match (major, minor) {
            (Some(major), _) if major >= 5 => true,
            (Some(4), Some(minor)) => *family == "opus" && minor >= 7,
            _ => false,
        }
    })
}

/// Recognizes the 400 a provider returns for budget thinking on a model that
/// only supports adaptive thinking: `"thinking.type.enabled" is not
/// supported for this model. Use "thinking.type.adaptive" ...`, possibly
/// relayed (and re-escaped) inside a gateway's own error envelope.
pub fn is_adaptive_thinking_rejection(body: &str) -> bool {
    body.contains("thinking.type.enabled") && body.contains("thinking.type.adaptive")
}

/// The adaptive-thinking counterpart of [`resolve_thinking`]: the same
/// request fields select it, but the model sizes its own thinking and an
/// explicit level travels as `output_config.effort`. A bare
/// `extended_thinking: true` leaves the effort at the provider default.
pub fn resolve_adaptive_thinking(
    extended_thinking: bool,
    reasoning_effort: Option<rusty_protocol::backend::ReasoningEffort>,
) -> Option<(AnthropicThinking, Option<AnthropicOutputConfig>)> {
    if !extended_thinking && reasoning_effort.is_none() {
        return None;
    }
    use rusty_protocol::backend::ReasoningEffort;
    let effort = reasoning_effort.map(|effort| AnthropicOutputConfig {
        // Anthropic's effort scale is low/medium/high/max; clamp the rest.
        effort: match effort {
            ReasoningEffort::Minimal | ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High | ReasoningEffort::XHigh => "high",
            ReasoningEffort::Max | ReasoningEffort::Ultra => "max",
        },
    });
    Some((
        AnthropicThinking {
            kind: "adaptive".to_string(),
            budget_tokens: None,
        },
        effort,
    ))
}

/// Resolves the outgoing `thinking` block from a [`ModelRequest`](rusty_model_backend::request::ModelRequest)'s
/// own `extended_thinking`/`reasoning_effort` fields, or `None` when neither
/// asks for it.
///
/// Both fields can request thinking independently -- `extended_thinking` is
/// a plain on/off flag (used when a caller wants thinking without picking a
/// level), `reasoning_effort` is a three-level enum (used when a caller
/// picked a level explicitly, e.g. from a model reference's own
/// `::reasoning=` suffix). An explicit level scales the budget; a bare
/// `extended_thinking: true` with no level keeps the original behavior of
/// using the full available budget.
pub fn resolve_thinking(
    extended_thinking: bool,
    reasoning_effort: Option<rusty_protocol::backend::ReasoningEffort>,
    max_tokens: u64,
) -> Result<Option<AnthropicThinking>, ModelError> {
    if !extended_thinking && reasoning_effort.is_none() {
        return Ok(None);
    }
    if max_tokens < 2048 {
        return Err(ModelError::InvalidRequest {
            message: "extended thinking requires max_tokens >= 2048".to_string(),
        });
    }
    let full_budget = max_tokens - 1024;
    use rusty_protocol::backend::ReasoningEffort;
    let budget_tokens = match reasoning_effort {
        Some(ReasoningEffort::Minimal | ReasoningEffort::Low) => {
            (full_budget / 4).max(1024).min(full_budget)
        }
        Some(ReasoningEffort::Medium) => (full_budget / 2).max(1024).min(full_budget),
        Some(
            ReasoningEffort::High
            | ReasoningEffort::XHigh
            | ReasoningEffort::Max
            | ReasoningEffort::Ultra,
        )
        | None => full_budget,
    };
    Ok(Some(AnthropicThinking {
        kind: "enabled".to_string(),
        budget_tokens: Some(budget_tokens),
    }))
}
