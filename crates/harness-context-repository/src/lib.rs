#![warn(clippy::all)]

//! A [`ContextProvider`] that adds a few repository facts to the system prompt.
//!
//! It reads only the snapshots that are already published: it never scans, refreshes, calls a
//! model, or waits for a refresh. The block is one overview line plus facts for up to a few
//! indexed files named in the latest user message, truncated to a character budget, and is empty
//! (the request is left untouched) when there is no snapshot or nothing relevant.
mod facts;
mod mentions;

use std::sync::Arc;

use async_trait::async_trait;
use harness_context::ContextProvider;
use harness_git_analysis::GitAnalysis;
use harness_protocol::backend::ExecutionRequest;
use harness_runtime::traits::Workspace;
use harness_workspace::repository_intelligence::RepositoryGraph;

const HEADER: &str = "Repository facts (read-only, from the latest published snapshot; files edited since may differ):";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryContextConfig {
    /// Upper bound on the characters added to the system prompt.
    pub max_chars: usize,
    /// Upper bound on files described individually.
    pub max_files: usize,
}

impl Default for RepositoryContextConfig {
    fn default() -> Self {
        Self {
            max_chars: 1_500,
            max_files: 3,
        }
    }
}

/// Opt-in: construct it and add it to the context chain to enable it.
pub struct RepositoryContextProvider {
    graph: Arc<dyn RepositoryGraph>,
    git: Option<Arc<dyn GitAnalysis>>,
    config: RepositoryContextConfig,
}

impl RepositoryContextProvider {
    pub fn new(graph: Arc<dyn RepositoryGraph>) -> Self {
        Self {
            graph,
            git: None,
            config: RepositoryContextConfig::default(),
        }
    }

    /// Adds churn and co-change facts for described files.
    pub fn with_git(mut self, git: Arc<dyn GitAnalysis>) -> Self {
        self.git = Some(git);
        self
    }

    pub fn with_config(mut self, config: RepositoryContextConfig) -> Self {
        self.config = config;
        self
    }

    /// The text to add for `request`, or `None` when there is nothing worth adding.
    fn block_for(&self, request: &ExecutionRequest) -> Option<String> {
        let snapshot = self.graph.current()?;
        let git = self.git.as_ref().and_then(|git| git.current());
        let message = mentions::latest_user_text(&request.messages);
        let files = mentions::mentioned_files(&message, &snapshot.imports, self.config.max_files);
        let mut lines = vec![HEADER.to_owned(), facts::overview_line(&snapshot)];
        lines.extend(
            files
                .iter()
                .map(|file| facts::file_line(&snapshot, git.as_deref(), file)),
        );
        Some(fit(lines, self.config.max_chars))
    }
}

/// Joins `lines`, dropping whole trailing lines (never cutting one mid-way) to fit `max_chars`.
fn fit(lines: Vec<String>, max_chars: usize) -> String {
    let mut text = String::new();
    for line in lines {
        let extra = line.chars().count() + usize::from(!text.is_empty());
        if text.chars().count() + extra > max_chars {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line);
    }
    text
}

#[async_trait]
impl ContextProvider for RepositoryContextProvider {
    async fn assemble(&self, mut request: ExecutionRequest, _: &dyn Workspace) -> ExecutionRequest {
        if let Some(block) = self.block_for(&request).filter(|b| !b.is_empty()) {
            request.system_prompt = if request.system_prompt.is_empty() {
                block
            } else {
                format!("{}\n\n{block}", request.system_prompt)
            };
        }
        request
    }
}
