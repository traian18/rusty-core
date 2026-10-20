use harness_code_health::{Category, Finding, Severity};

use crate::{
    doc::yaml::Node_,
    file::InfraFile,
    graph::cycles,
    kind::InfraKind,
    rule::{code, finding, rule, InfraConfig, InfraContext, InfraRule},
};

/// `uses: owner/action@ref` where the ref is not a full commit SHA.
pub struct UnpinnedAction;

fn is_sha(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

/// Every `uses:` value in a workflow or composite action, with its line.
fn uses(node: &Node_, out: &mut Vec<(String, u32)>) {
    for entry in node.entries() {
        if entry.key == "uses" {
            if let Some(value) = entry.value.as_str() {
                out.push((value.to_owned(), entry.key_line));
            }
        } else {
            uses(&entry.value, out);
        }
    }
    for item in node.items() {
        uses(item, out);
    }
}

impl InfraRule for UnpinnedAction {
    fn id(&self) -> &'static str {
        "gha_unpinned_action"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::Security
    }
    fn summary(&self) -> &'static str {
        "An action is referenced by a tag or branch instead of a commit SHA."
    }
    fn applies(&self, kind: InfraKind) -> bool {
        kind == InfraKind::GithubActions
    }

    fn check(&self, file: &InfraFile<'_>, _: &InfraContext<'_>, _: &InfraConfig) -> Vec<Finding> {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for document in &yaml.documents {
            uses(document, &mut found);
        }
        let mut out = Vec::new();
        for (value, line) in found {
            // Local actions and container images are not fetched by tag from a repository.
            if value.starts_with("./") || value.starts_with("docker://") {
                continue;
            }
            let Some((action, reference)) = value.split_once('@') else {
                continue;
            };
            if is_sha(reference) {
                continue;
            }
            let first_party = action.starts_with("actions/") || action.starts_with("github/");
            let branch = matches!(reference, "main" | "master" | "latest" | "develop");
            let severity = if branch {
                Severity::High
            } else if first_party {
                Severity::Low
            } else {
                Severity::Medium
            };
            out.push(finding(
                self,
                file,
                severity,
                (line, line),
                action,
                format!(
                    "{} is pinned to {}, which its owner can move; pin the full commit SHA.",
                    code(action),
                    code(reference)
                ),
            ));
        }
        out
    }
}

/// The workflow's top-level mapping.
fn workflow<'a>(file: &'a InfraFile<'_>) -> Option<&'a Node_> {
    file.yaml.as_ref()?.documents.first()
}

/// `(name, job node)` for each job.
fn jobs(workflow: &Node_) -> Vec<(&str, &Node_)> {
    workflow
        .get("jobs")
        .map(|j| {
            j.entries()
                .iter()
                .map(|e| (e.key.as_str(), &e.value))
                .collect()
        })
        .unwrap_or_default()
}

/// Every step of every job.
fn steps(workflow: &Node_) -> Vec<&Node_> {
    jobs(workflow)
        .into_iter()
        .flat_map(|(_, job)| job.get("steps").map(|s| s.items()).unwrap_or(&[]))
        .collect()
}

rule!(
    BroadPermissions,
    "gha_broad_permissions",
    Category::Security,
    "A workflow grants write-all permissions or never limits the token's permissions.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let grants_all = |node: &Node_| node.as_str() == Some("write-all");
        let mut scoped = false;
        for entry in wf.entries().iter().filter(|e| e.key == "permissions") {
            scoped = true;
            if grants_all(&entry.value) {
                out.push(finding(
                    this,
                    file,
                    Severity::High,
                    (entry.key_line, entry.key_line),
                    "",
                    "`permissions: write-all` gives every job full write access; list only the scopes each job needs.".to_owned(),
                ));
            }
        }
        for (name, job) in jobs(wf) {
            for entry in job.entries().iter().filter(|e| e.key == "permissions") {
                scoped = true;
                if grants_all(&entry.value) {
                    out.push(finding(
                        this,
                        file,
                        Severity::High,
                        (entry.key_line, entry.key_line),
                        name,
                        format!(
                            "Job {} grants `write-all`; list only the scopes it needs.",
                            code(name)
                        ),
                    ));
                }
            }
        }
        if !scoped && file.path.contains("/workflows/") {
            out.push(finding(
                this,
                file,
                Severity::Low,
                (1, 1),
                "",
                "No `permissions:` block, so the token gets the repository default (often write); set `permissions: contents: read` and widen per job.".to_owned(),
            ));
        }
        out
    }
);

const INJECTABLE: [&str; 8] = [
    "github.event.issue.title",
    "github.event.issue.body",
    "github.event.pull_request.title",
    "github.event.pull_request.body",
    "github.event.comment.body",
    "github.event.review.body",
    "github.head_ref",
    "github.event.pull_request.head.ref",
];

rule!(
    ScriptInjection,
    "gha_script_injection",
    Category::Security,
    "Untrusted event data is expanded straight into a run script.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for step in steps(wf) {
            let Some(run) = step.entry("run") else {
                continue;
            };
            let Some(script) = run.value.as_str() else {
                continue;
            };
            for source in INJECTABLE {
                if script.contains(&format!("${{{{ {source}"))
                    || script.contains(&format!("${{{{{source}"))
                {
                    out.push(finding(
                        this,
                        file,
                        Severity::High,
                        (run.key_line, run.key_line),
                        source,
                        format!(
                            "{} is expanded into the script, so a crafted title or branch name runs as shell; pass it through `env:` and quote the variable.",
                            code(source)
                        ),
                    ));
                }
            }
        }
        out
    }
);

rule!(
    PullRequestTargetCheckout,
    "gha_pull_request_target_checkout",
    Category::Security,
    "A pull_request_target workflow checks out the pull request's own code with a privileged token.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let triggers = wf.get("on").or_else(|| wf.get("true"));
        let targeted = triggers.is_some_and(|t| {
            t.as_str() == Some("pull_request_target")
                || t.items().iter().any(|i| i.as_str() == Some("pull_request_target"))
                || t.entry("pull_request_target").is_some()
        });
        if !targeted {
            return Vec::new();
        }
        let mut out = Vec::new();
        for step in steps(wf) {
            let checkout = step
                .get("uses")
                .and_then(|u| u.as_str())
                .is_some_and(|u| u.starts_with("actions/checkout"));
            let head = step
                .get("with")
                .and_then(|w| w.get("ref"))
                .and_then(|r| r.as_str())
                .is_some_and(|r| r.contains("pull_request.head") || r.contains("head_ref"));
            if checkout && head {
                let line = step.get("uses").map_or(step.line, |u| u.line);
                out.push(finding(
                    this,
                    file,
                    Severity::Critical,
                    (line, line),
                    "",
                    "This `pull_request_target` workflow runs the pull request's code with secrets and write access; check out the base ref, or split the untrusted build into a `pull_request` workflow.".to_owned(),
                ));
            }
        }
        out
    }
);

rule!(
    MissingTimeout,
    "gha_missing_timeout",
    Category::Infrastructure,
    "A job has no timeout-minutes, so a hung step holds a runner for six hours.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let entries = wf.get("jobs").map(|j| j.entries()).unwrap_or(&[]);
        entries
            .iter()
            .filter(|e| {
                e.value.entry("timeout-minutes").is_none() && e.value.entry("uses").is_none()
            })
            .map(|e| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (e.key_line, e.key_line),
                    &e.key,
                    format!(
                        "Job {} has no `timeout-minutes`; set one close to its normal duration.",
                        code(&e.key)
                    ),
                )
            })
            .collect()
    }
);

const RETIRED_RUNNERS: [&str; 7] = [
    "ubuntu-18.04",
    "ubuntu-20.04",
    "macos-10.15",
    "macos-11",
    "macos-12",
    "windows-2016",
    "windows-2019",
];

rule!(
    DeprecatedRunner,
    "gha_deprecated_runner",
    Category::Infrastructure,
    "A job runs on a runner image GitHub has retired or is retiring.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (name, job) in jobs(wf) {
            let Some(entry) = job.entry("runs-on") else {
                continue;
            };
            let labels: Vec<&str> = match entry.value.as_str() {
                Some(one) => vec![one],
                None => entry
                    .value
                    .items()
                    .iter()
                    .filter_map(|i| i.as_str())
                    .collect(),
            };
            for label in labels.into_iter().filter(|l| RETIRED_RUNNERS.contains(l)) {
                out.push(finding(
                    this,
                    file,
                    Severity::Low,
                    (entry.key_line, entry.key_line),
                    name,
                    format!(
                        "Job {} runs on {}, which is retired; move to a current image.",
                        code(name),
                        code(label)
                    ),
                ));
            }
        }
        out
    }
);

rule!(
    LongRunScript,
    "gha_long_run_script",
    Category::Infrastructure,
    "A run step holds a long inline script that cannot be tested or reused.",
    [GithubActions],
    |this, file, config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let limit = config.limit("gha_long_run_script", "lines", 20);
        steps(wf)
            .into_iter()
            .filter_map(|s| s.entry("run"))
            .filter_map(|run| {
                let lines = run.value.as_str()?.lines().count() as u32;
                (lines > limit).then(|| {
                    finding(
                        this,
                        file,
                        Severity::Low,
                        (run.key_line, run.value.end_line),
                        "",
                        format!("A `run` script of {lines} lines (limit {limit}); move it to a script file in the repository."),
                    )
                })
            })
            .collect()
    }
);

/// `needs` of a job as job names.
fn needs(job: &Node_) -> Vec<String> {
    match job.get("needs") {
        Some(n) => match n.as_str() {
            Some(one) => vec![one.to_owned()],
            None => n
                .items()
                .iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect(),
        },
        None => Vec::new(),
    }
}

rule!(
    UnknownNeeds,
    "gha_unknown_needs",
    Category::Infrastructure,
    "A job needs a job that does not exist in the workflow, which GitHub rejects.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let entries = wf.get("jobs").map(|j| j.entries()).unwrap_or(&[]);
        let names: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        let mut out = Vec::new();
        for job in entries {
            let Some(line) = job.value.entry("needs").map(|n| n.key_line) else {
                continue;
            };
            for need in needs(&job.value) {
                if !names.contains(&need.as_str()) {
                    out.push(finding(
                        this,
                        file,
                        Severity::High,
                        (line, line),
                        &need,
                        format!(
                            "Job {} needs {}, which is not a job in this workflow.",
                            code(&job.key),
                            code(&need)
                        ),
                    ));
                }
            }
        }
        out
    }
);

rule!(
    NeedsCycle,
    "gha_needs_cycle",
    Category::Infrastructure,
    "Jobs need each other in a loop, so the workflow can never start them.",
    [GithubActions],
    |this, file, _config| {
        let Some(wf) = workflow(file) else {
            return Vec::new();
        };
        let entries = wf.get("jobs").map(|j| j.entries()).unwrap_or(&[]);
        let edges = entries
            .iter()
            .map(|e| (e.key.clone(), needs(&e.value)))
            .collect();
        let mut out = Vec::new();
        for component in cycles(&edges) {
            for job in entries.iter().filter(|e| component.contains(&e.key)) {
                let line = job
                    .value
                    .entry("needs")
                    .map_or(job.key_line, |n| n.key_line);
                out.push(finding(
                    this,
                    file,
                    Severity::High,
                    (line, line),
                    &job.key,
                    format!(
                        "Job {} is part of a `needs` loop with {}.",
                        code(&job.key),
                        component
                            .iter()
                            .filter(|j| **j != job.key)
                            .map(|j| code(j))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
        out
    }
);

rule!(
    MissingLocalReference,
    "gha_missing_local_reference",
    Category::Infrastructure,
    "A step or job uses a local workflow or action that is not in the repository.",
    [GithubActions],
    |this, file, _config| {
        if file.repo_files.is_empty() {
            return Vec::new();
        }
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for document in &yaml.documents {
            uses(document, &mut found);
        }
        found
            .into_iter()
            .filter_map(|(value, line)| {
                let path = value
                    .strip_prefix("./")?
                    .split('@')
                    .next()?
                    .trim_end_matches('/');
                let exists = if path.ends_with(".yml") || path.ends_with(".yaml") {
                    file.repo_files.iter().any(|f| f == path)
                } else {
                    let prefix = format!("{path}/action.");
                    file.repo_files.iter().any(|f| f.starts_with(&prefix))
                };
                (!exists).then(|| {
                    finding(
                        this,
                        file,
                        Severity::High,
                        (line, line),
                        path,
                        format!("{} does not exist in the repository.", code(&value)),
                    )
                })
            })
            .collect()
    }
);
