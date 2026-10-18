//! The optional signals around a target: Git history and findings. Each is read only when it was
//! asked for and its toolset is available; otherwise it is reported as missing, never guessed.
use std::sync::Arc;

use harness_findings::{FindingSet, Severity};
use harness_git_analysis::{
    ChurnQuery, ChurnScope, ChurnSort, CoChangeQuery, GitQueryError, GitSnapshot,
};
use harness_workspace::repository_intelligence::{RepositorySnapshot, RootRelativePath};
use serde_json::{json, Value};

use super::{
    facts::path_text,
    request::{Request, Section},
};
use crate::graph::GraphContext;

const CO_CHANGE_PARTNERS: usize = 3;
const TOP_FINDINGS: usize = 5;
const TOP_CHURN: usize = 5;

pub(super) struct Signals {
    git: Option<(Arc<dyn harness_git_analysis::GitAnalysis>, Arc<GitSnapshot>)>,
    findings: Option<Arc<FindingSet>>,
    /// Sections that were asked for and could not be served, with the reason.
    pub missing: Vec<(&'static str, &'static str)>,
}

impl Signals {
    pub fn gather(
        context: &GraphContext,
        snapshot: &RepositorySnapshot,
        request: &Request,
    ) -> Self {
        let mut missing = Vec::new();
        let git = request
            .includes(Section::History)
            .then(|| match &context.git {
                None => {
                    if request.explicit_include {
                        missing.push(("history", "the Git toolset is not enabled"));
                    }
                    None
                }
                Some(git) => match git.current() {
                    Some(current) => Some((git.clone(), current)),
                    None => {
                        if request.explicit_include {
                            missing.push(("history", "no Git snapshot yet; call repo_git.refresh"));
                        }
                        None
                    }
                },
            });
        let findings = request.includes(Section::Findings).then(|| match &context.findings {
            None => {
                if request.explicit_include {
                    missing.push(("findings", "the findings toolset is not enabled"));
                }
                None
            }
            Some(findings) => match findings.current() {
                Some(set) if set.repository_snapshot == snapshot.id => Some(set),
                Some(_) => {
                    if request.explicit_include {
                        missing.push(("findings", "findings were computed for another snapshot; call repo_findings.analyze"));
                    }
                    None
                }
                None => {
                    if request.explicit_include {
                        missing.push(("findings", "no findings yet; call repo_findings.analyze"));
                    }
                    None
                }
            },
        });
        Self {
            git: git.flatten(),
            findings: findings.flatten(),
            missing,
        }
    }

    pub fn git_snapshot(&self) -> Option<String> {
        self.git.as_ref().map(|(_, snapshot)| snapshot.id.0.clone())
    }

    pub fn has_history(&self) -> bool {
        self.git.is_some()
    }

    pub fn has_findings(&self) -> bool {
        self.findings.is_some()
    }

    /// Churn and the strongest co-change partners of one file; correlation, not dependency.
    pub fn history_of_file(&self, path: &RootRelativePath) -> Option<Value> {
        let (git, snapshot) = self.git.as_ref()?;
        let churn = git
            .churn(&ChurnQuery {
                snapshot: snapshot.id.clone(),
                scope: ChurnScope::File(path.clone()),
                sort: ChurnSort::Commits,
                cursor: None,
                limit: 1,
            })
            .ok()?
            .entries
            .into_iter()
            .next()?;
        let partners = match git.co_changed(&CoChangeQuery { snapshot: snapshot.id.clone(), file: path.clone(), min_support: None, min_confidence_per_mille: None, cursor: None, limit: CO_CHANGE_PARTNERS }) {
            Ok(page) => page.entries.into_iter().map(|p| json!({"file": path_text(&p.partner), "support": p.support, "confidence_per_mille": p.confidence_per_mille})).collect::<Vec<_>>(),
            Err(GitQueryError::UnknownFile(_)) => Vec::new(),
            Err(_) => return None,
        };
        Some(json!({
            "label": "historical correlation, not a dependency or ownership",
            "commits": churn.commits,
            "contributors": churn.contributors,
            "last_change": churn.last_change,
            "co_changed_with": partners,
        }))
    }

    /// The most changed files under a folder.
    pub fn history_of_folder(&self, folder: &RootRelativePath) -> Option<Value> {
        let (git, snapshot) = self.git.as_ref()?;
        let page = git
            .churn(&ChurnQuery {
                snapshot: snapshot.id.clone(),
                scope: ChurnScope::Prefix(folder.clone()),
                sort: ChurnSort::Commits,
                cursor: None,
                limit: TOP_CHURN,
            })
            .ok()?;
        Some(json!({
            "label": "historical correlation, not a dependency or ownership",
            "most_changed": page.entries.iter().map(|e| json!({"file": path_text(&e.file), "commits": e.commits, "last_change": e.last_change})).collect::<Vec<_>>(),
        }))
    }

    /// Co-change hints for roots of a blast radius, kept apart from the import results.
    pub fn co_change_hints(&self, roots: &[&RootRelativePath]) -> Option<Value> {
        let (git, snapshot) = self.git.as_ref()?;
        let files: Vec<Value> = roots
            .iter()
            .take(5)
            .filter_map(|root| {
                let page = git.co_changed(&CoChangeQuery { snapshot: snapshot.id.clone(), file: (*root).clone(), min_support: None, min_confidence_per_mille: None, cursor: None, limit: 5 }).ok()?;
                Some(json!({"file": path_text(root), "partners": page.entries.iter().map(|p| json!({"file": path_text(&p.partner), "support": p.support})).collect::<Vec<_>>()}))
            })
            .collect();
        Some(json!({"label": "historical correlation, not a dependency", "files": files}))
    }

    /// Findings whose subjects include any of `files`: counts by severity and the top few.
    pub fn findings_of(&self, files: &[&RootRelativePath]) -> Option<Value> {
        let set = self.findings.as_ref()?;
        let relevant: Vec<_> = set
            .findings
            .iter()
            .filter(|f| f.subjects.iter().any(|s| files.contains(&s)))
            .collect();
        let count = |severity: Severity| relevant.iter().filter(|f| f.severity == severity).count();
        Some(json!({
            "total": relevant.len(),
            "by_severity": {"high": count(Severity::High), "medium": count(Severity::Medium), "low": count(Severity::Low), "info": count(Severity::Info)},
            "top": relevant.iter().take(TOP_FINDINGS).map(|f| json!({"id": f.id, "rule": f.rule.id, "severity": f.severity, "title": f.title})).collect::<Vec<_>>(),
        }))
    }
}
