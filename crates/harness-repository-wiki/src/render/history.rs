//! History and findings facts, shared by every level. History is labelled as correlation.
use harness_findings::Severity;
use harness_git_analysis::{ChurnQuery, ChurnScope, ChurnSort, CoChangeQuery, GitQueryError};
use harness_workspace::repository_intelligence::RootRelativePath;

use super::{Cx, SectionBuilder, SHORT_LIMIT};
use crate::{
    facts::path_text,
    model::{Citation, PageId, Section},
};

const CO_CHANGE_PARTNERS: usize = 3;

pub(super) fn file_history(cx: &Cx<'_>, file: &RootRelativePath) -> Option<Section> {
    let (git, snapshot) = cx.git.as_ref()?;
    let churn = git
        .churn(&ChurnQuery {
            snapshot: snapshot.id.clone(),
            scope: ChurnScope::File(file.clone()),
            sort: ChurnSort::Commits,
            cursor: None,
            limit: 1,
        })
        .ok()?
        .entries
        .into_iter()
        .next()?;
    let mut section = SectionBuilder::new(
        "history",
        "History (correlation, not ownership or dependency)",
        SHORT_LIMIT,
    );
    section.push(
        format!(
            "{} commits by {} contributors",
            churn.commits, churn.contributors
        ),
        Vec::new(),
        Vec::new(),
    );
    match git.co_changed(&CoChangeQuery {
        snapshot: snapshot.id.clone(),
        file: file.clone(),
        min_support: None,
        min_confidence_per_mille: None,
        cursor: None,
        limit: CO_CHANGE_PARTNERS,
    }) {
        Ok(page) => {
            for partner in page.entries {
                section.push(
                    format!(
                        "often changed with {} ({} of {} commits)",
                        path_text(&partner.partner),
                        partner.support,
                        partner.file_commits
                    ),
                    Vec::new(),
                    vec![PageId::file(&path_text(&partner.partner))],
                );
            }
        }
        Err(GitQueryError::UnknownFile(_)) => {}
        Err(_) => return None,
    }
    Some(section.finish())
}

pub(super) fn folder_history(cx: &Cx<'_>, folder: &RootRelativePath) -> Option<Section> {
    let (git, snapshot) = cx.git.as_ref()?;
    let page = git
        .churn(&ChurnQuery {
            snapshot: snapshot.id.clone(),
            scope: ChurnScope::Prefix(folder.clone()),
            sort: ChurnSort::Commits,
            cursor: None,
            limit: SHORT_LIMIT,
        })
        .ok()?;
    let mut section = SectionBuilder::new(
        "history",
        "Most changed (correlation, not ownership)",
        SHORT_LIMIT,
    );
    for entry in page.entries {
        section.push(
            format!("{} — {} commits", path_text(&entry.file), entry.commits),
            Vec::new(),
            vec![PageId::file(&path_text(&entry.file))],
        );
    }
    Some(section.finish())
}

/// Findings whose subjects include any of `files`: a count line and the top few, each citing its
/// first location.
pub(super) fn findings(cx: &Cx<'_>, files: &[&RootRelativePath]) -> Option<Section> {
    let set = cx.findings.as_ref()?;
    let relevant: Vec<_> = set
        .findings
        .iter()
        .filter(|f| f.subjects.iter().any(|s| files.contains(&s)))
        .collect();
    if relevant.is_empty() {
        return None;
    }
    let count = |severity: Severity| relevant.iter().filter(|f| f.severity == severity).count();
    let mut section = SectionBuilder::new("findings", "Findings", SHORT_LIMIT);
    section.push(
        format!(
            "{} findings (high {}, medium {}, low {}, info {})",
            relevant.len(),
            count(Severity::High),
            count(Severity::Medium),
            count(Severity::Low),
            count(Severity::Info)
        ),
        Vec::new(),
        Vec::new(),
    );
    for finding in relevant.iter().take(SHORT_LIMIT - 1) {
        let citations = finding
            .locations
            .first()
            .and_then(|location| {
                let hash = cx.facts.file(&location.file)?.content_hash.clone();
                let line = location.line?;
                Some(vec![Citation {
                    file: path_text(&location.file),
                    start_line: line,
                    end_line: line,
                    content_hash: hash,
                }])
            })
            .unwrap_or_default();
        section.push(
            format!(
                "{} ({}, rule {})",
                finding.title,
                format!("{:?}", finding.severity).to_lowercase(),
                finding.rule.id
            ),
            citations,
            Vec::new(),
        );
    }
    section.note_omitted(relevant.len().saturating_sub(SHORT_LIMIT - 1));
    Some(section.finish())
}
