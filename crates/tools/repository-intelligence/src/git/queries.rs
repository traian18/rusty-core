use async_trait::async_trait;
use harness_git_analysis::{
    ChurnQuery, ChurnScope, ChurnSort, CoChangeQuery, ContributorQuery, GitCursor, GitSnapshotId,
};
use harness_tools::CancellationToken;
use serde::Deserialize;
use serde_json::{json, Value};

use super::GitContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

const INTERPRETATION_NOTE: &str = "Results are historical correlation within the analysed window, not causation, dependency, or code ownership.";

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Repository,
    File,
    Prefix,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChurnArguments {
    git_snapshot: Option<String>,
    scope: Option<Scope>,
    path: Option<String>,
    sort: Option<ChurnSort>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_git.churn`
pub(super) struct Churn(pub GitContext);

#[async_trait]
impl Operation for Churn {
    fn name(&self) -> &'static str {
        "repo_git.churn"
    }

    fn description(&self) -> &'static str {
        "File churn from Git history: commits, lines added and removed, contributors, first and last change. Scope is the whole repository, one file, or a directory prefix; sortable and paged. Historical correlation only."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{
            "git_snapshot":{"type":"string"},
            "scope":{"type":"string","enum":["repository","file","prefix"]},
            "path":{"type":"string","description":"Workspace-relative file or directory for scope=file/prefix"},
            "sort":{"type":"string","enum":["commits","lines_changed","last_changed","path"]},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: ChurnArguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.git_snapshot)?;
        let path = || -> Result<_, Failure> {
            args::file(
                args.path
                    .as_deref()
                    .ok_or_else(|| Failure::invalid("path is required for this scope"))?,
            )
        };
        let scope = match args.scope.unwrap_or(Scope::Repository) {
            Scope::Repository => ChurnScope::Repository,
            Scope::File => ChurnScope::File(path()?),
            Scope::Prefix => ChurnScope::Prefix(path()?),
        };
        let page = self.0.git.churn(&ChurnQuery {
            snapshot: snapshot.id.clone(),
            scope,
            sort: args.sort.unwrap_or(ChurnSort::Commits),
            cursor: args::optional_cursor::<GitCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(50),
        })?;
        let result = json!({"entries": page.entries, "next_cursor": args::next_cursor(&page.next_cursor), "interpretation": INTERPRETATION_NOTE});
        Ok(self.0.success(&snapshot, result))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoChangeArguments {
    git_snapshot: Option<String>,
    file: String,
    min_support: Option<u32>,
    min_confidence_per_mille: Option<u32>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_git.co_changed`
pub(super) struct CoChanged(pub GitContext);

#[async_trait]
impl Operation for CoChanged {
    fn name(&self) -> &'static str {
        "repo_git.co_changed"
    }

    fn description(&self) -> &'static str {
        "Files that changed in the same commits as a file, strongest first, with support, confidence (per mille) and example commits. A correlation in history, not an import or dependency relation. Thresholds can only be raised."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["file"],"properties":{
            "git_snapshot":{"type":"string"},
            "file":{"type":"string"},
            "min_support":{"type":"integer","minimum":1},
            "min_confidence_per_mille":{"type":"integer","minimum":0,"maximum":1000},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: CoChangeArguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.git_snapshot)?;
        let page = self.0.git.co_changed(&CoChangeQuery {
            snapshot: snapshot.id.clone(),
            file: args::file(&args.file)?,
            min_support: args.min_support,
            min_confidence_per_mille: args.min_confidence_per_mille,
            cursor: args::optional_cursor::<GitCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(20),
        })?;
        let result = json!({"file": page.file, "entries": page.entries, "next_cursor": args::next_cursor(&page.next_cursor), "interpretation": INTERPRETATION_NOTE});
        Ok(self.0.success(&snapshot, result))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributorArguments {
    git_snapshot: Option<String>,
    file: String,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_git.contributors`
pub(super) struct Contributors(pub GitContext);

#[async_trait]
impl Operation for Contributors {
    fn name(&self) -> &'static str {
        "repo_git.contributors"
    }

    fn description(&self) -> &'static str {
        "Recent contributors to a file: each author's share of commits and changed lines in the analysed window (display name plus a hash of the email). This is not code ownership."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["file"],"properties":{
            "git_snapshot":{"type":"string"},
            "file":{"type":"string"},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: ContributorArguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.git_snapshot)?;
        let page = self.0.git.contributors(&ContributorQuery {
            snapshot: snapshot.id.clone(),
            file: args::file(&args.file)?,
            cursor: args::optional_cursor::<GitCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(20),
        })?;
        let result = json!({"file": page.file, "definition": page.definition, "entries": page.entries, "next_cursor": args::next_cursor(&page.next_cursor), "interpretation": INTERPRETATION_NOTE});
        Ok(self.0.success(&snapshot, result))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageArguments {
    git_snapshot: Option<String>,
}

/// `repo_git.coverage`
pub(super) struct Coverage(pub GitContext);

#[async_trait]
impl Operation for Coverage {
    fn name(&self) -> &'static str {
        "repo_git.coverage"
    }

    fn description(&self) -> &'static str {
        "What the Git analysis covered and left out: commits examined and used, skipped merges, excluded and bulk commits, shallow history, uncommitted changes, why the walk stopped, and files that did not match the repository snapshot."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"git_snapshot":{"type":"string"}},"additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: CoverageArguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.git_snapshot)?;
        let coverage = self.0.git.coverage(&GitSnapshotId(snapshot.id.0.clone()))?;
        Ok(self.0.success(&snapshot, json!(coverage)))
    }
}
