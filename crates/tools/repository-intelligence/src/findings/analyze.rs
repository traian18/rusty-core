use async_trait::async_trait;
use harness_findings::AnalyzeRequest;
use harness_git_analysis::GitSnapshotId;
use harness_tools::CancellationToken;
use harness_workspace::repository_intelligence::SnapshotId;
use serde::Deserialize;
use serde_json::{json, Value};

use super::FindingsContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    snapshot: Option<String>,
    git_snapshot: Option<String>,
    use_git: Option<bool>,
}

/// `repo_findings.analyze`: computes a finding set from the chosen snapshots.
pub(super) struct Analyze(pub FindingsContext);

#[async_trait]
impl Operation for Analyze {
    fn name(&self) -> &'static str {
        "repo_findings.analyze"
    }

    fn description(&self) -> &'static str {
        "Compute ranked findings from the current (or chosen) repository snapshot and Git snapshot: import cycles, undeclared dependencies, unattached files and configured boundary violations (measurable), plus labelled candidates (unreferenced files, implicit coupling, change hotspots, unused declared dependencies). Returns the finding set id and a summary; candidates are heuristics, never facts."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{
            "snapshot":{"type":"string","description":"Repository snapshot id; defaults to the current snapshot"},
            "git_snapshot":{"type":"string","description":"Git snapshot id; defaults to the current Git snapshot"},
            "use_git":{"type":"boolean","description":"Use Git history for the rules that need it; default true"}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let set = self
            .0
            .findings
            .analyze(
                AnalyzeRequest {
                    repository_snapshot: args.snapshot.map(SnapshotId),
                    git_snapshot: args.git_snapshot.map(GitSnapshotId),
                    use_git: args.use_git.unwrap_or(true),
                },
                cancel.clone(),
            )
            .await?;
        let result = json!({"finding_set": set.id, "rules": set.rules, "summary": set.summary});
        Ok(self.0.success(&set, result))
    }
}
