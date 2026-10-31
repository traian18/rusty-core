//! Research: a step that cannot go on without an answer asks a read-only research step.
//!
//! The asking step calls `request_research`; the call is handed to the step's own event loop, which
//! runs the research as a child flow (it holds what a child run needs: the step's context, its
//! permissions and its questions to the user) and sends the findings back as the tool result.

mod registry;
mod run;
mod tool;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) use registry::WithResearch;
pub(super) use tool::ResearchTool;

/// The tool a step uses to send a question to a research step.
pub const RESEARCH_TOOL: &str = "request_research";

/// What a research step may use: reading only, and never more than the step
/// that asked can use itself.
pub(super) const RESEARCH_TOOLS: &[&str] = &[
    "read_file",
    "fs.read",
    "open_document",
    "list_files",
    "search_codebase",
    "workspace.search",
    "project_info",
    "repo_context",
    "repo_wiki",
    "web_search",
    "web_fetch",
    "web_extract",
];

pub(super) const RESEARCH_INSTRUCTIONS: &str = "You are a research step. A step of a running workflow cannot go on without an answer to the question in workflow_input.question; workflow_input.why (when given) says what that step needs it for, and workflow_input.request is the user's overall request, for orientation only.

What to do:
1. Find the answer in the project's files, asking the repository index before opening files when those tools are offered: repo_context for how files connect (several targets in one call) and the functions a file defines, repo_symbol for the body of one function or class (path::Name) instead of reading the whole file. Search for text the index does not hold (strings, configuration, documentation), then read only what the search points at.
2. Use the web only when the question is about something outside the project (a library, an API, a tool's behavior).
3. Do not change anything and do not run commands: you only read.
4. Be quick and stay on this one question; the step that asked is waiting for you.

What happens next: your reply goes back to the step that asked, as the result of its request_research call. Reply with:
## Answer
The direct answer, short and specific.
## Evidence
One line per fact: a file path with line numbers, or a URL.
## Unknowns
What you could not establish, or \"None\".";

/// A question a step sent with `request_research`, waiting for its answer.
pub(super) struct ResearchCall {
    pub(super) question: String,
    pub(super) why: Option<String>,
    /// Cancelled when the tool call ends early (timed out or cancelled).
    pub(super) cancel: CancellationToken,
    pub(super) reply: oneshot::Sender<Result<String, String>>,
}
