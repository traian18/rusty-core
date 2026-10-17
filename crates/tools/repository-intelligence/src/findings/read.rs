use async_trait::async_trait;
use harness_findings::{
    Confidence, FindingId, FindingKind, FindingsCursor, FindingsListQuery, Severity,
};
use harness_tools::CancellationToken;
use harness_workspace::repository_intelligence::{PackageId, RootRelativePath};
use serde::Deserialize;
use serde_json::{json, Value};

use super::FindingsContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

const SET_DESCRIPTION: &str = "Finding set id from repo_findings.analyze; defaults to the latest";

fn set_property() -> Value {
    json!({"type":"string","description":SET_DESCRIPTION})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetOnly {
    finding_set: Option<String>,
}

/// `repo_findings.summary`
pub(super) struct Summary(pub FindingsContext);

#[async_trait]
impl Operation for Summary {
    fn name(&self) -> &'static str {
        "repo_findings.summary"
    }

    fn description(&self) -> &'static str {
        "Counts of findings by rule, kind and severity, plus the rules that were skipped for missing inputs, truncated by their bound, and the number suppressed."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"finding_set":set_property()},"additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: SetOnly = args::parse(arguments)?;
        let set = self.0.set(&args.finding_set)?;
        Ok(self
            .0
            .success(&set, json!({"finding_set": set.id, "summary": set.summary})))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    finding_set: Option<String>,
    #[serde(default)]
    rules: Vec<String>,
    kind: Option<FindingKind>,
    min_severity: Option<Severity>,
    min_confidence: Option<Confidence>,
    package: Option<String>,
    file: Option<String>,
    #[serde(default)]
    include_suppressed: bool,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_findings.list`
pub(super) struct List(pub FindingsContext);

#[async_trait]
impl Operation for List {
    fn name(&self) -> &'static str {
        "repo_findings.list"
    }

    fn description(&self) -> &'static str {
        "List findings in priority order (kind, severity, confidence, impact). Filter by rule, kind, minimum severity or confidence, package, or file; suppressed findings are excluded unless requested. Paged."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{
            "finding_set":set_property(),
            "rules":{"type":"array","items":{"type":"string"}},
            "kind":{"type":"string","enum":["measurable","candidate"]},
            "min_severity":{"type":"string","enum":["high","medium","low","info"],"description":"Keep findings at least this severe"},
            "min_confidence":{"type":"string","enum":["high","medium","low"],"description":"Keep findings at least this confident"},
            "package":{"type":"string","description":"Package id"},
            "file":{"type":"string","description":"Workspace-relative file among the finding's subjects"},
            "include_suppressed":{"type":"boolean"},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":200}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: ListArguments = args::parse(arguments)?;
        let set = self.0.set(&args.finding_set)?;
        let file = args.file.as_deref().map(args::file).transpose()?;
        let page = self.0.findings.list(&FindingsListQuery {
            set: set.id.clone(),
            rules: args.rules,
            kind: args.kind,
            min_severity: args.min_severity,
            min_confidence: args.min_confidence,
            package: args.package.map(PackageId),
            file: file.map(|f: RootRelativePath| f),
            include_suppressed: args.include_suppressed,
            cursor: args::optional_cursor::<FindingsCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(50),
        })?;
        let result = json!({"finding_set": set.id, "entries": page.entries, "next_cursor": args::next_cursor(&page.next_cursor)});
        Ok(self.0.success(&set, result))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindingArguments {
    finding_set: Option<String>,
    finding: String,
}

fn finding_schema() -> Value {
    json!({"type":"object","required":["finding"],"properties":{
        "finding_set":set_property(),
        "finding":{"type":"string","description":"Finding id, as returned by repo_findings.list"}},
        "additionalProperties":false})
}

/// `repo_findings.get`
pub(super) struct Get(pub FindingsContext);

#[async_trait]
impl Operation for Get {
    fn name(&self) -> &'static str {
        "repo_findings.get"
    }

    fn description(&self) -> &'static str {
        "One finding with its severity, confidence, rationale, evidence and source locations (line and column when the file still matches the snapshot)."
    }

    fn schema(&self) -> Value {
        finding_schema()
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: FindingArguments = args::parse(arguments)?;
        let set = self.0.set(&args.finding_set)?;
        let entry = self.0.findings.finding(&set.id, &FindingId(args.finding))?;
        Ok(self.0.success(&set, json!(entry)))
    }
}

/// `repo_findings.explain`
pub(super) struct Explain(pub FindingsContext);

#[async_trait]
impl Operation for Explain {
    fn name(&self) -> &'static str {
        "repo_findings.explain"
    }

    fn description(&self) -> &'static str {
        "Explain a finding: the rule that produced it, its rationale, and its evidence expanded into the full import edges. Candidates are heuristics, not facts."
    }

    fn schema(&self) -> Value {
        finding_schema()
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: FindingArguments = args::parse(arguments)?;
        let set = self.0.set(&args.finding_set)?;
        let explanation = self.0.findings.explain(&set.id, &FindingId(args.finding))?;
        Ok(self.0.success(&set, json!(explanation)))
    }
}
