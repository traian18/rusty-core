//! Builder, handle, and error types for the public harness session API.
//!
//! This module provides three primary types:
//!
//! * [`SessionBuilder`] — a fluent builder for configuring and starting
//!   sessions.
//! * [`SessionHandle`] — a handle to a live session returned by
//!   [`SessionBuilder::start()`].
//! * [`HarnessError`] — error type covering validation failures and
//!   runtime errors.

use std::sync::Arc;

use tokio::sync::broadcast;

use harness_protocol::commands::UserInput;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;

use harness_runtime::session_client::{SessionClient, SessionSnapshot};
use harness_runtime::session_runtime::{SessionCommand, SessionError, SessionRuntime};
use harness_runtime::traits::{EventSink, ExecutionBackend, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;

// ---------------------------------------------------------------------------
// BroadcastEventSink — bridges an EventSink to a broadcast::Sender
// ---------------------------------------------------------------------------

/// An [`EventSink`] implementation that forwards every event to a
/// [`broadcast::Sender<AgentEventEnvelope>`] so external subscribers
/// can receive session events.
struct BroadcastEventSink {
    tx: broadcast::Sender<AgentEventEnvelope>,
}

impl EventSink for BroadcastEventSink {
    fn send(&self, envelope: AgentEventEnvelope) {
        let _ = self.tx.send(envelope);
    }
}

// ---------------------------------------------------------------------------
// HarnessError
// ---------------------------------------------------------------------------

/// Errors that can occur during session construction or operation.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The `backend` field was not set before calling [`SessionBuilder::start`].
    #[error("missing required field: backend")]
    MissingBackend,

    /// The `tools` (tool_registry) field was not set before calling
    /// [`SessionBuilder::start`].
    #[error("missing required field: tool_registry")]
    MissingToolRegistry,

    /// A session‑level runtime error occurred (e.g. invalid state, cancelled).
    #[error("session error: {0}")]
    Session(#[from] SessionError),
}

// ---------------------------------------------------------------------------
// SessionBuilder
// ---------------------------------------------------------------------------

/// A fluent builder for creating and starting harness sessions.
///
/// ## Example
///
/// ```ignore
/// let handle = Harness::new()
///     .session()
///     .backend(backend)
///     .tools(tool_registry)
///     .start()
///     .await?;
/// ```
pub struct SessionBuilder {
    /// The execution backend to use for this session.
    backend: Option<Arc<dyn ExecutionBackend>>,
    /// The tool registry to use for this session.
    tool_registry: Option<Arc<dyn ToolRegistry>>,
    /// The workspace binding to use for this session.
    ///
    /// Optional — defaults to an empty [`FakeWorkspace`] if not set, since
    /// real workspace implementations arrive in Phase 4 (spec §6.2, §37).
    workspace: Option<Arc<dyn Workspace>>,
}

impl SessionBuilder {
    /// Create a new, empty [`SessionBuilder`].
    ///
    /// All fields are initially `None` and must be set via the builder
    /// methods before calling [`start()`](SessionBuilder::start).
    pub fn new() -> Self {
        Self {
            backend: None,
            tool_registry: None,
            workspace: None,
        }
    }

    /// Set the execution backend for this session.
    ///
    /// This is required — [`start()`](SessionBuilder::start) will return
    /// [`HarnessError::MissingBackend`] if this is not set.
    pub fn backend(mut self, backend: Arc<dyn ExecutionBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Set the tool registry for this session.
    ///
    /// This is required — [`start()`](SessionBuilder::start) will return
    /// [`HarnessError::MissingToolRegistry`] if this is not set.
    pub fn tools(mut self, tool_registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(tool_registry);
        self
    }

    /// Set the workspace binding for this session.
    ///
    /// Optional — if not set, [`start()`](SessionBuilder::start) defaults to
    /// an empty in-memory [`FakeWorkspace`], matching the Phase 2 scope
    /// (real workspace implementations arrive in Phase 4).
    pub fn workspace(mut self, workspace: Arc<dyn Workspace>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Consume the builder and start a new session.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::MissingBackend`] if
    /// [`backend()`](SessionBuilder::backend) was not called.
    ///
    /// Returns [`HarnessError::MissingToolRegistry`] if
    /// [`tools()`](SessionBuilder::tools) was not called.
    pub async fn start(self) -> Result<SessionHandle, HarnessError> {
        let backend = self
            .backend
            .ok_or(HarnessError::MissingBackend)?;
        let tool_registry = self
            .tool_registry
            .ok_or(HarnessError::MissingToolRegistry)?;
        let workspace: Arc<dyn Workspace> = self
            .workspace
            .unwrap_or_else(|| Arc::new(FakeWorkspace::new()));

        let session_id = SessionId::new();

        // Create a broadcast channel for external subscribers.
        let (event_tx, _) = broadcast::channel::<AgentEventEnvelope>(256);

        // Wrap the broadcast sender in an EventSink.
        let event_sink = Arc::new(BroadcastEventSink { tx: event_tx.clone() });

        // Construct the session runtime with the provided backend,
        // tool registry, workspace, and event sink.
        let runtime = Arc::new(SessionRuntime::new(
            session_id,
            backend,
            tool_registry,
            workspace,
            event_sink,
        ));

        // Create the session client that wraps the runtime.
        let client = SessionClient::new(runtime);

        Ok(SessionHandle {
            client,
            session_id,
        })
    }
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SessionHandle
// ---------------------------------------------------------------------------

/// A handle to a live session returned by [`SessionBuilder::start()`].
///
/// Provides convenience methods that delegate to the underlying
/// [`SessionClient`]:
///
/// * [`send()`](SessionHandle::send) — send a plain‑text prompt.
/// * [`subscribe()`](SessionHandle::subscribe) — subscribe to the event stream.
/// * [`snapshot()`](SessionHandle::snapshot) — take a lightweight read snapshot.
/// * [`session_id()`](SessionHandle::session_id) — return the session's ID.
pub struct SessionHandle {
    /// The underlying session client.
    client: SessionClient,
    /// The session's unique identifier.
    session_id: SessionId,
}

impl SessionHandle {
    /// Send a plain‑text prompt to the session's root agent.
    ///
    /// The prompt is wrapped in a [`SessionCommand::Prompt`] and routed
    /// through the [`SessionClient`].
    pub async fn send(&self, prompt: &str) -> Result<(), HarnessError> {
        let command = SessionCommand::Prompt(UserInput {
            text: prompt.to_string(),
            attachments: vec![],
        });
        self.client.send(command).await?;
        Ok(())
    }

    /// Subscribe to the session's event stream.
    ///
    /// Returns a new [`broadcast::Receiver`] that observes all
    /// [`AgentEventEnvelope`]s produced by the session.  Each call
    /// creates an independent subscriber.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEventEnvelope> {
        self.client.subscribe()
    }

    /// Take a lightweight read snapshot of the current session state.
    ///
    /// The snapshot is a pure projection of live runtime state — no
    /// stored copy is kept.
    pub fn snapshot(&self) -> SessionSnapshot {
        self.client.snapshot()
    }

    /// Return the unique identifier for this session.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}
