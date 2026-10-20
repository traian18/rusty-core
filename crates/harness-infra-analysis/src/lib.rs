#![warn(clippy::all)]

//! Infrastructure analysis: deterministic rules over Terraform, Terragrunt, CI pipelines,
//! Dockerfiles, Kubernetes manifests and the other files a repository runs on. Rules report
//! the same [`Finding`]s as code health, so scoring and the Code Health page treat them alike.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `kind` | Which kind of infrastructure file a path is |
//! | `doc` | Parsed models with line numbers: HCL, YAML, Dockerfile |
//! | `file` | One file parsed for its kind |
//! | `rule` | The `InfraRule` trait, configuration and the finding helper |
//! | `graph` | Terraform modules and Terragrunt units related across files |
//! | `history` | Git-history findings for files without functions |
//! | `rules` | One module per pack, the registry and `check_file` |
//! | `scoring` | A scoring configuration that includes the rules |
//!
//! Suppress a finding with `# rusty-ignore: rule_id` on its line or the line above.
pub mod doc;
mod file;
pub mod graph;
mod history;
mod kind;
mod rule;
mod rules;
mod scoring;

pub use file::InfraFile;
pub use graph::{InfraGraph, InfraOverview};
pub use harness_code_health::{Category, Finding, Severity};
pub use history::history_findings;
pub use kind::{classify, is_candidate, maybe_yaml_family, InfraKind, ALL as ALL_KINDS};
pub use rule::{InfraConfig, InfraContext, InfraRule};
pub use rules::{check_file, registry};
pub use scoring::scoring_config;

/// Parses `text` as the kind `path` is and runs the enabled rules; `None` for other files.
pub fn analyse(
    path: &str,
    text: &str,
    context: &InfraContext<'_>,
    config: &InfraConfig,
) -> Option<(InfraKind, Vec<Finding>)> {
    let file = InfraFile::new(path, text)?.with_repo_files(context.repo_files.unwrap_or(&[]));
    let findings = check_file(&file, context, config);
    Some((file.kind, findings))
}
