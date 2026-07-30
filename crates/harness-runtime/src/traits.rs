//! Core runtime traits for execution backends, tool executors, tool registries,
//! workspaces, and event sinks.
//!
//! These are the **behavioral** traits that give life to the wire-format types
//! defined in [`harness-protocol`].  The protocol crate holds only serializable
//! data structures ([`ExecutionRequest`], [`ExecutionEvent`], [`ToolCall`],
//! [`AgentEventEnvelope`], etc.); the traits here drive the actual async I/O,
//! cancellation, and streaming that implement those contracts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::tools::{ToolCall, ToolDescriptor, ToolError, ToolResult};

// ---------------------------------------------------------------------------
// ExecutionBackend
// ---------------------------------------------------------------------------

/// An execution backend that processes agent requests against a language model.
///
/// Implementations wrap a specific provider (Anthropic, OpenAI, etc.) and are
/// responsible for:
///
/// * Serializing the [`ExecutionRequest`] into the provider's wire format.
/// * Streaming back normalized [`ExecutionEvent`]s (text deltas, reasoning,
///   tool call requests, usage updates, final result, or errors).
/// * Observing the [`CancellationToken`] so that in-flight requests can be
///   aborted promptly.
///
/// # Object safety
///
/// This trait is `dyn`-compatible — all methods take `&self` and return owned
/// values or futures.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Returns the backend's descriptor, including its identity and capabilities.
    fn descriptor(&self) -> BackendDescriptor;

    /// Returns a summary of the backend's capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// Execute a request against this backend.
    ///
    /// The backend streams [`ExecutionEvent`]s into `sink` as they occur
    /// and returns the final [`ExecutionResult`] once execution completes
    /// (or an [`ExecutionError`] if something went wrong).
    ///
    /// # Cancellation
    ///
    /// When `cancel` is triggered the backend should stop work as soon as
    /// practical and return [`ExecutionError::Cancelled`].
    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError>;
}

// ---------------------------------------------------------------------------
// ToolExecutor
// ---------------------------------------------------------------------------

/// Executes a single tool call and produces a result.
///
/// Each tool (e.g. `fs.read`, `bash`, `web_search`) is represented by an
/// implementation of this trait.  Tools are stateless from the harness's
/// perspective — any state they need should be captured at construction time
/// and stored in the implementor.
///
/// # Object safety
///
/// This trait is `dyn`-compatible.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute the given tool call and return its result.
    ///
    /// # Cancellation
    ///
    /// When `cancel` is triggered the tool should stop work as soon as
    /// practical and return [`ToolError::Timeout`].
    async fn execute(
        &self,
        call: ToolCall,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// A registry of available tool executors, indexed by tool name.
///
/// The harness uses this trait to look up tools when processing
/// [`ExecutionEvent::ToolCallRequested`] events.  Registries may be
/// session-scoped (containing only the tools granted to that session) or
/// global (containing every tool the runtime knows about).
///
/// # Object safety
///
/// This trait is `dyn`-compatible.
#[async_trait]
pub trait ToolRegistry: Send + Sync {
    /// Look up a tool executor by its name (e.g. `"fs.read"`).
    ///
    /// Returns `None` if no tool with that name is registered.
    fn lookup(&self, name: &str) -> Option<Arc<dyn ToolExecutor>>;

    /// Returns descriptors for all registered tools.
    fn descriptors(&self) -> Vec<ToolDescriptor>;
}

// ---------------------------------------------------------------------------
// EventSink
// ---------------------------------------------------------------------------

/// A sink for receiving agent event envelopes.
///
/// Implementations are responsible for fanning events out to session-level
/// subscribers, persisting them, or forwarding them to external observers.
/// The canonical implementation publishes onto a
/// [`broadcast::Sender<AgentEventEnvelope>`] but custom sinks may layer on
/// filtering, encryption, or audit logging.
///
/// # Object safety
///
/// This trait is `dyn`-compatible.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Send an event envelope for processing.
    ///
    /// Implementations **should not** block indefinitely; if the underlying
    /// channel is full the implementation should decide whether to drop the
    /// event, buffer it, or apply backpressure.
    fn send(&self, envelope: AgentEventEnvelope);
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// A query used to search a [`Workspace`].
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// The text/regex pattern to search for.
    pub pattern: String,
    /// Restrict the search to files under this path prefix, if given.
    pub path_prefix: Option<PathBuf>,
    /// Cap the number of matches returned.
    pub max_results: Option<usize>,
}

/// A single match produced by a workspace search.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    /// The path of the file containing the match.
    pub path: PathBuf,
    /// The 1-based line number of the match, if known.
    pub line: u64,
    /// A short preview of the matching content.
    pub preview: String,
}

/// The result of a workspace search.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// All matches found, in implementation-defined order.
    pub matches: Vec<SearchMatch>,
}

/// A snapshot of high-level workspace status (spec §37).
#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatus {
    /// The workspace root, if the implementation has one.
    pub root: Option<PathBuf>,
    /// The number of files currently tracked/visible in the workspace.
    pub file_count: usize,
    /// Whether writes are currently permitted.
    pub is_writable: bool,
}

/// Errors that can occur during workspace operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkspaceError {
    /// The requested path does not exist in the workspace.
    #[error("path not found: {0}")]
    NotFound(String),

    /// The operation is not permitted by the current workspace policy.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// An underlying I/O error occurred.
    #[error("workspace I/O error: {0}")]
    Io(String),
}

/// An abstract project/filesystem environment (spec §37).
///
/// Agents never touch `std::fs` directly — all file and search access is
/// mediated through an injected `Workspace` implementation. This allows the
/// same agent logic to run against a real filesystem, an IDE's unsaved
/// buffers, a read-only snapshot, or (in tests) a purely in-memory fake.
///
/// # Object safety
///
/// This trait is `dyn`-compatible.
///
/// # Phase 2 status
///
/// Only [`FakeWorkspace`](crate::workspace::FakeWorkspace) exists as of
/// Phase 2. Real implementations (`FsWorkspace`, `IdeWorkspace`, ...) arrive
/// in Phase 4, at which point this trait definition is expected to migrate
/// to the `harness-workspace` crate (see `TASKS-03` trade-offs).
#[async_trait]
pub trait Workspace: Send + Sync {
    /// Read the full contents of a file at `path`.
    async fn read(&self, path: &Path) -> Result<Vec<u8>, WorkspaceError>;

    /// Write `data` to a file at `path`, creating or overwriting it.
    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), WorkspaceError>;

    /// Search the workspace for content matching `query`.
    async fn search(&self, query: SearchQuery) -> Result<SearchResult, WorkspaceError>;

    /// Return a snapshot of the workspace's current status.
    async fn status(&self) -> Result<WorkspaceStatus, WorkspaceError>;
}
