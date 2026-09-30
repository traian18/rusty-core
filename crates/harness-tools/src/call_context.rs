//! Ambient identity of the tool call an executor is currently serving.
//!
//! `ToolExecutor::execute` deliberately receives only the call's arguments,
//! so built-in tools stay oblivious to protocol identifiers. Executors that
//! forward work elsewhere (the IDE's `HostToolExecutor`) still need the
//! model-facing call ID to let the other side correlate its own work with
//! the `ToolCallRequested`/`ToolCallCompleted` events for that call. The
//! runtime scopes every `execute` future with the ID; forwarding executors
//! read it back with [`current_tool_call_id`]. The session the call belongs
//! to is scoped the same way ([`current_tool_session_id`]): sessions that
//! share one tool registry -- an orchestration step's isolated session and
//! its parent -- would otherwise be indistinguishable to such an executor.

use std::future::Future;

tokio::task_local! {
    static CURRENT_TOOL_CALL_ID: String;
    static CURRENT_TOOL_SESSION_ID: String;
}

/// Runs `future` with `call_id` visible to [`current_tool_call_id`].
pub async fn with_tool_call_id<F: Future>(call_id: String, future: F) -> F::Output {
    CURRENT_TOOL_CALL_ID.scope(call_id, future).await
}

/// Runs `future` with both the call ID and the ID of the session that
/// requested the call visible to executors.
pub async fn with_tool_call<F: Future>(
    call_id: String,
    session_id: String,
    future: F,
) -> F::Output {
    CURRENT_TOOL_SESSION_ID
        .scope(session_id, CURRENT_TOOL_CALL_ID.scope(call_id, future))
        .await
}

/// The ID of the session whose agent requested the tool call being
/// executed, when the runtime scoped it (see [`with_tool_call`]).
pub fn current_tool_session_id() -> Option<String> {
    CURRENT_TOOL_SESSION_ID.try_with(Clone::clone).ok()
}

/// The model-facing ID of the tool call being executed, when the executor
/// was invoked by the runtime (absent in direct/unit-test invocations).
pub fn current_tool_call_id() -> Option<String> {
    CURRENT_TOOL_CALL_ID.try_with(Clone::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_scoped_call_id_is_visible_only_inside_the_scope() {
        assert_eq!(current_tool_call_id(), None);
        let seen = with_tool_call_id("call-1".to_string(), async { current_tool_call_id() }).await;
        assert_eq!(seen.as_deref(), Some("call-1"));
        assert_eq!(current_tool_call_id(), None);
    }

    #[tokio::test]
    async fn the_calling_session_is_scoped_with_the_call() {
        let seen = with_tool_call("call-1".into(), "session-1".into(), async {
            (current_tool_call_id(), current_tool_session_id())
        })
        .await;
        assert_eq!(seen, (Some("call-1".into()), Some("session-1".into())));
        assert_eq!(current_tool_session_id(), None);
    }
}
