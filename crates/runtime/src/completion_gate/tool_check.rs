//! A check that runs a tool, optionally handing it the proposed answer.

use rusty_protocol::commands::VerdictOutcome;

use rusty_protocol::tools::PermissionMode;

use serde_json::{json, Value};

use tokio_util::sync::CancellationToken;

use super::{GateContext, MAX_FEEDBACK_CHARS};

use crate::completion_gate::text::truncate;

/// `args` with the proposed answer under `answer_arg`, when the check names one.
pub(super) fn with_answer(args: Value, answer_arg: Option<&str>, answer: &str) -> Value {
    let Some(name) = answer_arg else {
        return args;
    };
    let mut object = match args {
        Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    object.insert(name.to_owned(), Value::String(answer.to_owned()));
    Value::Object(object)
}

/// Run a tool directly. The gate acts for the harness, not the model, so
/// the profile's tool scope does not apply — but the session's permission
/// does: only tools the session allows without approval may be used, since
/// there is no one to answer a prompt mid-gate.
pub(super) async fn run_tool(
    context: &GateContext,
    tool: &str,
    args: Value,
    cancel: CancellationToken,
) -> VerdictOutcome {
    match context.tool_permissions.get(tool) {
        Some(PermissionMode::Allow) => {}
        Some(_) => {
            return VerdictOutcome::Error {
                message: format!("tool {tool} needs approval in this session"),
            }
        }
        None => {
            return VerdictOutcome::Error {
                message: format!("tool {tool} is not available in this session"),
            }
        }
    }
    let Some(executor) = context.tool_registry.get_executor(tool) else {
        return VerdictOutcome::Error {
            message: format!("tool {tool} is not registered"),
        };
    };
    let arguments = if args.is_null() { json!({}) } else { args };
    match executor
        .execute(rusty_tool_api::ToolInput { arguments }, cancel)
        .await
    {
        Ok(result) if !result.is_error => VerdictOutcome::Pass,
        Ok(result) => {
            let output = match result.output {
                Value::String(text) => text,
                other => other.to_string(),
            };
            VerdictOutcome::Fail {
                feedback: truncate(&format!("{tool} failed:\n{output}"), MAX_FEEDBACK_CHARS),
            }
        }
        Err(error) => VerdictOutcome::Error {
            message: format!("{tool} could not run: {error:?}"),
        },
    }
}
