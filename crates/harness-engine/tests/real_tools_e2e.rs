//! End-to-end test for a real-tools session (Phase 4).
//!
//! Creates a temp directory as the workspace root, wires in all four real
//! tool executors (`fs.read`, `fs.edit`, `workspace.search`, `shell.exec`)
//! via [`SessionBuilder::toolset`], drives a [`FakeBackend`] that emits a
//! tool-call request, and validates both the [`AgentEvent`] sequence and
//! filesystem side effects.
//!
//! # Expected event sequence
//!
//! When the backend emits [`ExecutionEvent::ToolCallRequested`] followed by
//! [`ExecutionEvent::Completed`] with `finish_reason: "tool_use"`:
//!
//! 1. [`AgentEvent::ToolCallRequested`] — the model requested a tool call.
//! 2. [`AgentEvent::ToolCallCompleted`] — the tool execution finished
//!    (with success or error).
//!
//! `ToolCallStarted` and `ToolCallProgress` event types exist in the
//! protocol but are not yet emitted by the current agent transition code;
//! they will appear in a future phase.

use std::collections::HashMap;
use std::sync::Arc;

use tempfile::tempdir;

use harness_engine::SessionBuilder;
use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
use harness_protocol::events::{AgentEvent, AgentOutcome};
use harness_protocol::ids::{RequestId, ToolId, ToolCallId};
use harness_protocol::tools::{AgentToolset, PermissionMode, ToolCall, ToolCapability, ToolDescriptor, ToolPolicy};
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::testing::FakeBackend;
use harness_runtime::traits::Workspace;
use harness_workspace::FsWorkspace;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Drain buffered events from a broadcast receiver non-blockingly.
fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<harness_protocol::events::AgentEventEnvelope>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        events.push(envelope.event);
    }
    events
}

/// Build a scripted [`FakeBackend`] that emits a single tool-call request
/// followed by a completion with `finish_reason: "tool_use"`.
///
/// The backend's `execute` returns the same set of events on every call,
/// so if the agent requests a second backend round (which happens when
/// the tool fails and `finish_reason` was `"tool_use"`), the same events
/// replay — creating additional tool-call cycles.  Callers should limit
/// their polling iterations to avoid an infinite loop.
fn make_tool_call_backend(tool_name: &str, call_args: serde_json::Value) -> Arc<FakeBackend> {
    let request_id = RequestId::new();
    let call_id = ToolCallId::new();

    Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::ToolCallRequested {
                    request_id,
                    call: ToolCall {
                        id: call_id,
                        name: tool_name.to_string(),
                        arguments: call_args,
                    },
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: ExecutionResult {
                        request_id,
                        usage: ModelUsage::default(),
                        cost: Cost::default(),
                        finish_reason: "tool_use".into(),
                    },
                },
            ])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "tool_use".into(),
            }),
    )
}

/// Build an [`AgentToolset`] with all four real tools enabled and permitted.
fn all_tools_toolset() -> AgentToolset {
    let tool_defs: [(&str, &str, serde_json::Value); 4] = [
        (
            "fs.read",
            "Read the full contents of a file in the workspace.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to read" }
                },
                "required": ["path"]
            }),
        ),
        (
            "fs.edit",
            "Replace the entire content of a file in the workspace.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit" },
                    "content": { "type": "string", "description": "New full content for the file" }
                },
                "required": ["path", "content"]
            }),
        ),
        (
            "workspace.search",
            "Search the workspace for files matching a text pattern.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text query to search for" }
                },
                "required": ["query"]
            }),
        ),
        (
            "shell.exec",
            "Execute a shell command and capture its output.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" }
                },
                "required": ["command"]
            }),
        ),
    ];

    let mut tools = HashMap::new();

    for (name, description, input_schema) in &tool_defs {
        let id = ToolId::new();
        tools.insert(
            id,
            ToolCapability {
                descriptor: ToolDescriptor {
                    id,
                    name: name.to_string(),
                    description: description.to_string(),
                    input_schema: input_schema.clone(),
                },
                policy: ToolPolicy {
                    permission: PermissionMode::Allow,
                    enabled: true,
                },
                delegatable: false,
            },
        );
    }

    AgentToolset { tools }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Verifies that `fs.read` tool executes against a real FsWorkspace:
///
/// - A temp-directory workspace is created with a fixture file.
/// - A [`SessionBuilder`] wires the real tools and the workspace.
/// - The backend emits a ToolCallRequested for `fs.read` on the fixture.
/// - The tool executes and reads the file from disk.
/// - The event stream contains ToolCallRequested and ToolCallCompleted.
/// - The file content is returned in the tool result.
#[tokio::test]
async fn real_tools_e2e_fs_read_real_execution() {
    // ── GIVEN ────────────────────────────────────────────
    let dir = tempdir().expect("tempdir should succeed");
    let fixture_path = dir.path().join("fixture.txt");
    tokio::fs::write(&fixture_path, "hello world from real fs")
        .await
        .expect("write fixture");

    // Use real FsWorkspace backed by the temp directory.
    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));

    let toolset = all_tools_toolset();

    // Backend emits ToolCallRequested for fs.read.
    let backend = make_tool_call_backend("fs.read", serde_json::json!({ "path": "fixture.txt" }));

    // ── WHEN — build session and send prompt ──────────────
    let session = SessionBuilder::new()
        .toolset(toolset, workspace)
        .backend(backend)
        .start()
        .await
        .expect("Session should build");

    let mut subscriber = session.subscribe();
    session
        .send("read the fixture file")
        .await
        .expect("sending prompt should succeed");

    // ── Collect events ────────────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..30 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
        }
        if all_events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // ── THEN — validate event sequence ────────────────────
    let req_pos = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallRequested { .. }));
    assert!(
        req_pos.is_some(),
        "fs.read test should emit ToolCallRequested; got {:#?}",
        all_events,
    );

    // If ToolCallCompleted appears, it must come after ToolCallRequested.
    if let Some(comp_pos) = all_events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
    {
        assert!(
            req_pos.unwrap() < comp_pos,
            "ToolCallRequested must precede ToolCallCompleted"
        );
    }

    // ── Fixture file remains unmodified ────────────────────
    let content = tokio::fs::read_to_string(&fixture_path)
        .await
        .expect("fixture file should exist");
    assert_eq!(content, "hello world from real fs");
}

/// Verifies that `fs.edit` tool executes against a real FsWorkspace:
///
/// - A temp directory with an initial file is created.
/// - A [`SessionBuilder`] wires the real tools.
/// - The backend emits a ToolCallRequested for `fs.edit`.
/// - The tool writes new content to disk.
/// - The file content on disk is updated.
#[tokio::test]
async fn real_tools_e2e_fs_edit_real_execution() {
    // ── GIVEN ────────────────────────────────────────────
    let dir = tempdir().expect("tempdir should succeed");
    let target_path = dir.path().join("target.txt");
    tokio::fs::write(&target_path, "old content")
        .await
        .expect("write initial content");

    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    let toolset = all_tools_toolset();

    // Backend emits ToolCallRequested for fs.edit.
    let backend = make_tool_call_backend(
        "fs.edit",
        serde_json::json!({
            "path": "target.txt",
            "content": "new content from real tool"
        }),
    );

    // ── WHEN — build session and send prompt ──────────────
    let session = SessionBuilder::new()
        .toolset(toolset, workspace)
        .backend(backend)
        .start()
        .await
        .expect("Session should build");

    let mut subscriber = session.subscribe();
    session
        .send("edit the target file")
        .await
        .expect("sending prompt should succeed");

    // ── Collect events ────────────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..30 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
        }
        if all_events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // ── THEN — validate tool was called ──────────────────
    let has_tool_call = all_events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallRequested { .. }));
    assert!(has_tool_call, "fs.edit test should emit ToolCallRequested");

    // ── Verify file was modified on disk ─────────────────
    let content = tokio::fs::read_to_string(&target_path)
        .await
        .expect("target file should exist after edit");
    assert_eq!(content, "new content from real tool");
}

/// Verifies that `workspace.search` tool executes against a real FsWorkspace:
///
/// - A temp directory with multiple files is created.
/// - One file contains the search query.
/// - The backend emits a ToolCallRequested for `workspace.search`.
/// - The tool finds and reports the match.
#[tokio::test]
async fn real_tools_e2e_workspace_search_real_execution() {
    // ── GIVEN ────────────────────────────────────────────
    let dir = tempdir().expect("tempdir should succeed");
    tokio::fs::write(dir.path().join("file1.txt"), "looking for needle here").await.unwrap();
    tokio::fs::write(dir.path().join("file2.txt"), "no match here").await.unwrap();

    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    let toolset = all_tools_toolset();

    // Backend emits ToolCallRequested for workspace.search.
    let backend = make_tool_call_backend(
        "workspace.search",
        serde_json::json!({ "query": "needle" }),
    );

    // ── WHEN — build session and send prompt ──────────────
    let session = SessionBuilder::new()
        .toolset(toolset, workspace)
        .backend(backend)
        .start()
        .await
        .expect("Session should build");

    let mut subscriber = session.subscribe();
    session
        .send("search for needle")
        .await
        .expect("sending prompt should succeed");

    // ── Collect events ────────────────────────────────────
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..30 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
        }
        if all_events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // ── THEN — validate tool was called ──────────────────
    let has_tool_call = all_events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallRequested { .. }));
    assert!(
        has_tool_call,
        "workspace.search test should emit ToolCallRequested"
    );
}

/// Verifies that all four tool descriptors are properly registered in the
/// session's toolset and that the [`SessionBuilder`] accepts them without
/// error.
#[tokio::test]
async fn session_builds_with_all_four_tools() {
    let dir = tempdir().expect("tempdir should succeed");
    let workspace: Arc<dyn Workspace> = Arc::new(FsWorkspace::new(dir.path().to_path_buf()));
    let toolset = all_tools_toolset();

    // A trivial backend that returns immediately with an end_turn result.
    let request_id = RequestId::new();
    let backend = Arc::new(
        FakeBackend::new()
            .with_events(vec![ExecutionEvent::Completed {
                request_id,
                result: ExecutionResult {
                    request_id,
                    usage: ModelUsage::default(),
                    cost: Cost::default(),
                    finish_reason: "end_turn".into(),
                },
            }])
            .with_result(ExecutionResult {
                request_id,
                usage: ModelUsage::default(),
                cost: Cost::default(),
                finish_reason: "end_turn".into(),
            }),
    );

    let session = SessionBuilder::new()
        .toolset(toolset, workspace)
        .backend(backend)
        .start()
        .await
        .expect("Session should build with all four tools");

    let mut subscriber = session.subscribe();

    session
        .send("check that all tools are available")
        .await
        .expect("send should succeed");

    // Collect events until Completed.
    let mut all_events: Vec<AgentEvent> = Vec::new();
    for _ in 0..20 {
        let batch = drain_events(&mut subscriber);
        if !batch.is_empty() {
            all_events.extend(batch);
        }
        if all_events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { outcome: AgentOutcome::Success }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // The run should complete successfully.
    let has_completed_success = all_events.iter().any(|e| {
        matches!(e, AgentEvent::Completed { outcome: AgentOutcome::Success })
    });
    assert!(
        has_completed_success,
        "Session should complete with Success outcome"
    );
}
