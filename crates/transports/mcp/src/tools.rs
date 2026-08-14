//! The MCP tool surface: what an IDE sees in `tools/list`, and what each
//! one does when called.

use std::sync::Arc;

use harness_protocol::admission::{CommandId, MutationMetadata};
use harness_protocol::commands::UserInput;
use harness_protocol::ids::SessionId;
use harness_protocol::rpc::{MutationCommand, RpcRequestBody, RpcResponseBody};
use harness_runtime::rpc::RpcHandler;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::run::prompt_and_wait;
use crate::McpServeConfig;

pub(crate) const CREATE_SESSION: &str = "harness_create_session";
pub(crate) const PROMPT: &str = "harness_prompt";
pub(crate) const CANCEL: &str = "harness_cancel";
pub(crate) const LIST_SESSIONS: &str = "harness_list_sessions";

/// The `tools/list` payload.
///
/// Schemas are written by hand rather than derived: they are the contract an
/// IDE reads, they need prose an author chooses, and there are four of them.
pub(crate) fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": CREATE_SESSION,
                "description":
                    "Start a new agent session against this harness's configured workspace. \
                     Returns a session_id to pass to the other tools.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Optional label for the session, for your own reference."
                        }
                    }
                }
            },
            {
                "name": PROMPT,
                "description":
                    "Send a prompt to a session and wait for the agent's run to finish. \
                     Returns the assistant's reply along with a summary of any tools it ran. \
                     This blocks for the duration of the run.",
                "inputSchema": {
                    "type": "object",
                    "required": ["session_id", "prompt"],
                    "properties": {
                        "session_id": {"type": "string", "description": "From harness_create_session."},
                        "prompt": {"type": "string", "description": "What to ask the agent."}
                    }
                }
            },
            {
                "name": CANCEL,
                "description": "Cancel a session's in-flight run without closing the session.",
                "inputSchema": {
                    "type": "object",
                    "required": ["session_id"],
                    "properties": {
                        "session_id": {"type": "string"}
                    }
                }
            },
            {
                "name": LIST_SESSIONS,
                "description": "List sessions known to this harness, including restorable ones.",
                "inputSchema": {"type": "object", "properties": {}}
            }
        ]
    })
}

/// Result of a `tools/call`, in MCP's shape.
pub(crate) fn text_result(text: impl Into<String>, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text.into()}],
        "isError": is_error,
    })
}

pub(crate) async fn call(
    handler: &Arc<dyn RpcHandler>,
    config: &McpServeConfig,
    name: &str,
    arguments: &Value,
    cancel: &CancellationToken,
) -> Value {
    match name {
        CREATE_SESSION => create_session(handler, config).await,
        PROMPT => prompt(handler, config, arguments, cancel).await,
        CANCEL => cancel_run(handler, arguments).await,
        LIST_SESSIONS => list_sessions(handler).await,
        other => text_result(format!("unknown tool {other:?}"), true),
    }
}

async fn create_session(handler: &Arc<dyn RpcHandler>, config: &McpServeConfig) -> Value {
    // Integration, workspace, and toolset come from server configuration,
    // not from the tool call: an MCP client has no way to know which
    // provider this harness is wired to, and letting it choose a workspace
    // root would let any connected IDE point the agent at an arbitrary
    // directory.
    let response = handler
        .handle(
            None,
            RpcRequestBody::CreateSession {
                workspace_root: config.workspace_root.clone(),
                integration: config.integration.clone(),
                integration_config: config.integration_config.clone(),
                toolset: config.toolset.clone(),
                mcp_servers: Vec::new(),
                skills: config.skills.clone(),
            },
        )
        .await;

    match response {
        RpcResponseBody::SessionCreated { session_id } => text_result(
            format!("Created session {session_id}. Pass this as session_id to harness_prompt."),
            false,
        ),
        RpcResponseBody::Failure(error) => text_result(
            format!("could not create a session: {}", error.message),
            true,
        ),
        other => text_result(format!("unexpected response: {other:?}"), true),
    }
}

async fn prompt(
    handler: &Arc<dyn RpcHandler>,
    config: &McpServeConfig,
    arguments: &Value,
    cancel: &CancellationToken,
) -> Value {
    let Some(session_id) = parse_session_id(arguments) else {
        return text_result(
            "session_id is required and must be a valid session id",
            true,
        );
    };
    let Some(text) = arguments.get("prompt").and_then(Value::as_str) else {
        return text_result("prompt is required and must be a string", true);
    };

    let input = UserInput {
        text: text.to_owned(),
        attachments: vec![],
    };

    match prompt_and_wait(handler, session_id, input, config.prompt_timeout, cancel).await {
        Err(message) => text_result(message, true),
        Ok((transcript, end)) => {
            let mut body = transcript.text.trim().to_owned();
            if !transcript.tool_calls.is_empty() {
                body.push_str("\n\n--- tool calls ---\n");
                body.push_str(&transcript.tool_calls.join("\n"));
            }
            // Anything other than a clean success gets the reason appended,
            // so a caller is never left guessing why a reply looks short.
            if end.is_error() {
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                body.push_str(&format!("[{}]", end.describe()));
            } else if transcript.is_empty() {
                body.push_str("(the run completed without producing any output)");
            }
            text_result(body, end.is_error())
        }
    }
}

async fn cancel_run(handler: &Arc<dyn RpcHandler>, arguments: &Value) -> Value {
    let Some(session_id) = parse_session_id(arguments) else {
        return text_result(
            "session_id is required and must be a valid session id",
            true,
        );
    };

    let response = handler
        .handle(
            Some(session_id),
            RpcRequestBody::Mutate {
                metadata: MutationMetadata {
                    command_id: CommandId::new(),
                    session_id,
                    run_id: None,
                    expected_session_revision: None,
                    trace_id: None,
                },
                command: MutationCommand::Cancel,
            },
        )
        .await;

    match response {
        RpcResponseBody::Admission { result, .. } => {
            text_result(format!("cancel: {result:?}"), false)
        }
        RpcResponseBody::Failure(error) => text_result(error.message, true),
        other => text_result(format!("unexpected response: {other:?}"), true),
    }
}

async fn list_sessions(handler: &Arc<dyn RpcHandler>) -> Value {
    match handler.handle(None, RpcRequestBody::ListSessions).await {
        RpcResponseBody::SessionsListed { sessions } if sessions.is_empty() => {
            text_result("no sessions", false)
        }
        RpcResponseBody::SessionsListed { sessions } => {
            let listing = sessions
                .into_iter()
                .map(|session| {
                    format!(
                        "{}  {}  (updated {:?}, restorable: {})",
                        session.session_id, session.title, session.updated_at, session.restorable
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            text_result(listing, false)
        }
        RpcResponseBody::Failure(error) => text_result(error.message, true),
        other => text_result(format!("unexpected response: {other:?}"), true),
    }
}

fn parse_session_id(arguments: &Value) -> Option<SessionId> {
    arguments
        .get("session_id")
        .and_then(Value::as_str)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_tool_has_a_name_description_and_object_schema() {
        let definitions = tool_definitions();
        let tools = definitions["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 4);

        for tool in tools {
            let name = tool["name"].as_str().expect("name");
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "{name} has no description"
            );
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "{name} must take an object"
            );
        }
    }

    #[test]
    fn the_advertised_names_match_the_dispatch_constants() {
        let definitions = tool_definitions();
        let names: Vec<&str> = definitions["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec![CREATE_SESSION, PROMPT, CANCEL, LIST_SESSIONS]);
    }

    #[test]
    fn text_result_uses_mcps_content_block_shape() {
        let value = text_result("hello", false);
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "hello");
        assert_eq!(value["isError"], false);
    }

    #[test]
    fn session_ids_are_parsed_and_garbage_is_rejected() {
        let id = SessionId::new();
        assert_eq!(
            parse_session_id(&json!({"session_id": id.to_string()})),
            Some(id)
        );
        assert_eq!(parse_session_id(&json!({"session_id": "nonsense"})), None);
        assert_eq!(parse_session_id(&json!({})), None);
        // A non-string must not panic.
        assert_eq!(parse_session_id(&json!({"session_id": 42})), None);
    }
}
