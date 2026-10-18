#![warn(clippy::all)]

//! A [`ContextProvider`] that gives a model the repository documentation it most likely needs.
//!
//! Each request gets a stable overview of the repository and, when the prompt or the work in
//! progress names something real, excerpts of the pages about it, with the reason each was chosen.
//! Related pages are offered as one-line pointers. With nothing to anchor on, only the overview is
//! sent. The text is labelled as data, rebuilt from the published snapshots for every request and
//! never stored in history.
//!
//! The provider only reads snapshots that are already published: it never scans, refreshes, calls
//! a model, or waits for a refresh.
mod mentions;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_context::ContextProvider;
use harness_findings::FindingsAnalysis;
use harness_git_analysis::GitAnalysis;
use harness_protocol::backend::ExecutionRequest;
use harness_repository_wiki::{
    Centrality, ContextLedger, ContextPlanner, ContextRenderer, FocusSource, PageRenderer,
    PlanInput, RenderedContext, StaticFocus, TaskKind, Tier,
};
use harness_runtime::traits::Workspace;
use harness_workspace::repository_intelligence::{RepositoryGraph, SnapshotId};

const TOOL_HINT: &str = "For more, call repo_wiki (documentation pages) or repo_context (what depends on a file, what a change would reach).";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryContextConfig {
    /// Upper bound on the characters of retrieved pages added to the system prompt (the overview
    /// is separate and fixed). Hosts size it from the model's window.
    pub max_chars: usize,
    /// Send the repository overview even when the prompt anchors nothing.
    pub brief: bool,
    /// Add one line naming the pull tools. Set it only when the session has them.
    pub point_to_tool: bool,
}

impl Default for RepositoryContextConfig {
    fn default() -> Self {
        Self {
            max_chars: 6_000,
            brief: true,
            point_to_tool: false,
        }
    }
}

/// One page that was put in front of the model, for observability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InjectedPage {
    pub page: String,
    pub tier: &'static str,
    pub reason: String,
    pub chars: usize,
    pub excerpt: bool,
}

/// What one request was given, reported to the observer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InjectionReport {
    pub snapshot: String,
    pub conversation: String,
    pub run: String,
    pub abstained: bool,
    pub brief_chars: usize,
    pub retrieved_chars: usize,
    pub dropped: usize,
    pub unchanged: usize,
    pub pages: Vec<InjectedPage>,
}

/// Told what each request was given, so it can be logged and later measured.
pub trait ContextObserver: Send + Sync {
    fn injected(&self, report: &InjectionReport);
}

/// Opt-in: construct it and add it to the context chain to enable it.
pub struct RepositoryContextProvider {
    graph: Arc<dyn RepositoryGraph>,
    pages: PageRenderer,
    focus: Arc<dyn FocusSource>,
    task: TaskKind,
    config: RepositoryContextConfig,
    ledger: Arc<ContextLedger>,
    observer: Option<Arc<dyn ContextObserver>>,
    centrality: Mutex<Option<(SnapshotId, Arc<Centrality>)>>,
}

impl RepositoryContextProvider {
    pub fn new(graph: Arc<dyn RepositoryGraph>) -> Self {
        Self {
            pages: PageRenderer::new(graph.clone()),
            graph,
            focus: Arc::new(StaticFocus(Default::default())),
            task: TaskKind::General,
            config: RepositoryContextConfig::default(),
            ledger: Arc::new(ContextLedger::new()),
            observer: None,
            centrality: Mutex::new(None),
        }
    }

    /// Adds history to the pages.
    pub fn with_git(mut self, git: Arc<dyn GitAnalysis>) -> Self {
        self.pages = self.pages.with_git(git);
        self
    }

    /// Adds findings to the pages.
    pub fn with_findings(mut self, findings: Arc<dyn FindingsAnalysis>) -> Self {
        self.pages = self.pages.with_findings(findings);
        self
    }

    /// What the person or run is working on.
    pub fn with_focus(mut self, focus: Arc<dyn FocusSource>) -> Self {
        self.focus = focus;
        self
    }

    /// The kind of work, from the running profile or workflow step.
    pub fn with_task(mut self, task: TaskKind) -> Self {
        self.task = task;
        self
    }

    pub fn with_config(mut self, config: RepositoryContextConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn ContextObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Shares one ledger between providers, so a conversation's earlier runs are known.
    pub fn with_ledger(mut self, ledger: Arc<ContextLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    fn centrality(
        &self,
        snapshot: &harness_workspace::repository_intelligence::RepositorySnapshot,
    ) -> Arc<Centrality> {
        let Ok(mut cached) = self.centrality.lock() else {
            return Arc::new(Centrality::new(snapshot));
        };
        match cached.as_ref() {
            Some((id, centrality)) if *id == snapshot.id => centrality.clone(),
            _ => {
                let centrality = Arc::new(Centrality::new(snapshot));
                *cached = Some((snapshot.id.clone(), centrality.clone()));
                centrality
            }
        }
    }

    /// The text to add for `request`, or `None` when there is nothing worth adding.
    fn block_for(&self, request: &ExecutionRequest) -> Option<String> {
        let snapshot = self.graph.current()?;
        let message = mentions::latest_user_text(&request.messages);
        let focus = self.focus.focus();
        let planner = ContextPlanner::new(&snapshot, self.centrality(&snapshot));
        let plan = planner.plan(&PlanInput {
            message: &message,
            focus: &focus,
            task: self.task,
        });
        let mut renderer = ContextRenderer::new(self.pages.clone());
        if self.config.point_to_tool {
            renderer = renderer.with_tool_hint(TOOL_HINT);
        }
        let conversation = request
            .messages
            .first()
            .map(|m| format!("{:?}", m.id))
            .unwrap_or_default();
        let run = format!("{:?}", request.run_id);
        let rendered = renderer
            .render(
                &plan,
                self.config.max_chars,
                &self.ledger.seen(&conversation, &run),
            )
            .ok()?;
        self.ledger.record(&conversation, &run, &rendered.injected);
        self.report(
            &snapshot.id.0,
            conversation,
            run,
            &plan.abstained,
            &rendered,
        );
        let mut parts = Vec::new();
        if self.config.brief && !rendered.brief.is_empty() {
            parts.push(rendered.brief);
        }
        if !rendered.retrieved.is_empty() {
            parts.push(rendered.retrieved);
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    fn report(
        &self,
        snapshot: &str,
        conversation: String,
        run: String,
        abstained: &bool,
        rendered: &RenderedContext,
    ) {
        let Some(observer) = &self.observer else {
            return;
        };
        observer.injected(&InjectionReport {
            snapshot: snapshot.to_owned(),
            conversation,
            run,
            abstained: *abstained,
            brief_chars: rendered.brief.chars().count(),
            retrieved_chars: rendered.retrieved.chars().count(),
            dropped: rendered.dropped,
            unchanged: rendered.unchanged,
            pages: rendered
                .injected
                .iter()
                .map(|item| InjectedPage {
                    page: item.page.0.clone(),
                    tier: if item.tier == Tier::High {
                        "high"
                    } else {
                        "medium"
                    },
                    reason: item.reason.clone(),
                    chars: item.chars,
                    excerpt: item.excerpt,
                })
                .collect(),
        });
    }
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
