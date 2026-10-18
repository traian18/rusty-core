//! `repo_context`: one call that tells a model what a file or folder is connected to, what a change
//! would reach, and how to start in an unfamiliar folder. Every answer has the same shape and a
//! `status`; a language that cannot be read is an answer (`unsupported`), not an error.
//!
//! The tool composes the repository contracts (graph, groups, framework relations, Git, findings)
//! and adds no scanning of its own. It reads published snapshots only and never refreshes.
mod answer;
mod blast;
mod budget;
mod changes;
mod connections;
mod orient;
mod request;
mod signals;
mod support;
mod target;

use async_trait::async_trait;
use harness_repository_wiki::{facts, tests_of};
use harness_tools::CancellationToken;
use harness_workspace::{
    import_graph::ImportCapabilities, repository_intelligence::RepositorySnapshot,
};
use serde_json::{json, Value};
use std::sync::Arc;

use self::{
    answer::{next, refresh_next, Answer, Status},
    facts::{path_text, Facts},
    request::{Mode, Request},
    signals::Signals,
    target::{Absent, Target},
};
use crate::{
    failure::Failure,
    graph::GraphContext,
    operation::{Operation, Success},
};

/// Everything one answer reads, built once per call.
struct Cx<'a> {
    pub context: &'a GraphContext,
    pub snapshot: &'a Arc<RepositorySnapshot>,
    pub facts: Facts<'a>,
    pub request: &'a Request,
    pub signals: Signals,
    pub capabilities: ImportCapabilities,
}

pub(crate) struct RepoContext(pub GraphContext);

#[async_trait]
impl Operation for RepoContext {
    fn name(&self) -> &'static str {
        "repo_context"
    }

    fn description(&self) -> &'static str {
        "Understand code in one call. mode connections: what a file or folder imports, who imports it, framework links (routes, injections, renders, commands), tests, findings and history. blast_radius: what may break if it changes, by depth, with the entry points and tests it reaches. orient: a short map of a folder (its sub-folders and loose files, entry points, most depended-on files). changes: for changed files, the packages, routes and tests they touch. Every answer carries a status (ok, partial, unsupported, not_indexed, stale, not_found), what could be read for the target, a one-line summary and ready-to-run next calls. An unsupported language is a normal answer, not an error."
    }

    fn schema(&self) -> Value {
        request::schema()
    }

    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure> {
        let request = request::parse(arguments)?;
        let snapshot = self.0.snapshot(&request.snapshot)?;
        if cancel.is_cancelled() {
            return Err(Failure::cancelled());
        }
        let cx = Cx {
            context: &self.0,
            snapshot: &snapshot,
            facts: Facts::new(&snapshot),
            signals: Signals::gather(&self.0, &snapshot, &request),
            capabilities: self.0.graph.import_capabilities(),
            request: &request,
        };
        let answer = match request.mode {
            Mode::Changes => changes::answer(&cx)?,
            mode => {
                let target = target::resolve(&self.0.graph, &snapshot, &request.targets[0])?;
                match target {
                    Target::Absent { path, why, profile } => absent(&cx, &path, &why, profile),
                    target => match mode {
                        Mode::Connections => connections::answer(&cx, target)?,
                        Mode::BlastRadius => blast::answer(&cx, target)?,
                        _ => orient::answer(&cx, target)?,
                    },
                }
            }
        };
        let git_snapshot = answer.git_snapshot.clone();
        let truncation = answer.truncation();
        let mut success = self.0.success(&snapshot, truncation, answer.into_result());
        success.git_snapshot = git_snapshot;
        if request.includes(request::Section::Tests) {
            success
                .components
                .push((format!("rule:{}", tests_of::RULE), tests_of::RULE_VERSION));
        }
        Ok(success)
    }
}

/// The status of a readable target: stale when it changed on disk, partial when something asked
/// for could not be read, ok otherwise.
fn readable_status(cx: &Cx<'_>, changed: Option<&str>, unresolved: bool) -> Status {
    if changed.is_some() {
        Status::Stale
    } else if cx.snapshot.imports.partial || unresolved || !cx.signals.missing.is_empty() {
        Status::Partial
    } else {
        Status::Ok
    }
}

/// `support` for a target that can be read.
fn support_block(
    cx: &Cx<'_>,
    language: Option<String>,
    gaps: Vec<String>,
    frameworks: Vec<String>,
    changed: Option<&str>,
) -> Value {
    let mut gaps = gaps;
    if cx.snapshot.imports.partial {
        gaps.push("the snapshot's import coverage is partial".to_owned());
    }
    if let Some(change) = changed {
        gaps.push(change.to_owned());
    }
    let state = |on: bool, requested: bool| {
        if !requested {
            "not_requested"
        } else if on {
            "available"
        } else {
            "unavailable"
        }
    };
    json!({
        "imports": if cx.snapshot.imports.partial { "partial" } else { "resolved" },
        "language": language,
        "frameworks": frameworks,
        "symbols": "unavailable",
        "history": state(cx.signals.has_history(), cx.request.includes(request::Section::History)),
        "findings": state(cx.signals.has_findings(), cx.request.includes(request::Section::Findings)),
        "gaps": gaps,
        "missing": cx.signals.missing.iter().map(|(section, reason)| json!({"section": section, "reason": reason})).collect::<Vec<_>>(),
    })
}

/// The answer for something that cannot be read: why, and what could be done instead.
fn absent(
    cx: &Cx<'_>,
    path: &str,
    why: &Absent,
    profile: Option<harness_workspace::import_graph::LanguageProfile>,
) -> Answer {
    let target = json!({"kind": "path", "path": path, "language": profile.map(|p| support::profile_name(&p))});
    match why {
        Absent::Unsupported { language } => {
            let name = language.map_or("this file type".to_owned(), str::to_owned);
            Answer::absent(
                Status::Unsupported,
                json!({"kind": "file", "path": path, "language": language}),
                support::unsupported_block(&cx.capabilities, *language),
                format!("{name} is not supported, so {path} has no connections here; read the file instead."),
            )
        }
        Absent::NotIndexed { reason } => Answer::absent(
            Status::NotIndexed,
            target,
            json!({"imports": "not_indexed", "gaps": [reason]}),
            format!("{path} is not in the snapshot: {reason}."),
        ),
        Absent::Added => {
            let mut answer = Answer::absent(
                Status::Stale,
                target,
                json!({"imports": "not_indexed", "gaps": ["added since the snapshot was built"]}),
                format!("{path} exists but was added after the snapshot was built."),
            );
            answer
                .next
                .push(refresh_next("index files added since the last snapshot"));
            answer
        }
        Absent::NotFound => Answer::absent(
            Status::NotFound,
            target,
            json!({"imports": "unavailable", "gaps": []}),
            format!("nothing exists at {path} in the workspace; check the path."),
        ),
    }
}

/// A file as the `target` block describes it.
fn file_target(
    cx: &Cx<'_>,
    path: &harness_workspace::repository_intelligence::RootRelativePath,
    profile: harness_workspace::import_graph::LanguageProfile,
    package: Option<&harness_workspace::repository_intelligence::PackageId>,
) -> Value {
    json!({
        "kind": "file",
        "path": path_text(path),
        "language": support::profile_name(&profile),
        "package": cx.facts.package_name(package),
    })
}

/// A call to `repo_context` for follow-ups.
fn context_next(mode: &str, target: &str, why: impl Into<String>) -> Value {
    next("repo_context", json!({"mode": mode, "target": target}), why)
}
