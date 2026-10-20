//! Every rule, in a fixed order, and the call that runs them on a file.
mod ansible;
mod ci;
mod compose;
mod dockerfile;
mod github_actions;
mod kubernetes;
mod makefile;
mod repo;
mod secrets;
mod shell;
mod terraform;
mod terragrunt;

use harness_code_health::Finding;

use crate::{
    file::InfraFile,
    rule::{InfraConfig, InfraContext, InfraRule},
};

pub fn registry() -> Vec<Box<dyn InfraRule>> {
    vec![
        Box::new(terraform::UnpinnedProvider),
        Box::new(terraform::UnpinnedModule),
        Box::new(terraform::VariableHygiene),
        Box::new(terraform::OutputHygiene),
        Box::new(terraform::ProvisionerUse),
        Box::new(terraform::OpenIngress),
        Box::new(github_actions::UnpinnedAction),
        Box::new(github_actions::BroadPermissions),
        Box::new(github_actions::ScriptInjection),
        Box::new(github_actions::PullRequestTargetCheckout),
        Box::new(github_actions::MissingTimeout),
        Box::new(github_actions::DeprecatedRunner),
        Box::new(github_actions::LongRunScript),
        Box::new(dockerfile::LatestTag),
        Box::new(dockerfile::MissingUser),
        Box::new(dockerfile::AddInsteadOfCopy),
        Box::new(dockerfile::AptNoCleanup),
        Box::new(dockerfile::SecretInEnv),
        Box::new(dockerfile::CacheOrder),
        Box::new(kubernetes::LatestTag),
        Box::new(kubernetes::Privileged),
        Box::new(kubernetes::NoLimits),
        Box::new(kubernetes::NoProbes),
        Box::new(terraform::UnusedVariable),
        Box::new(terraform::UnusedLocal),
        Box::new(terraform::UnusedData),
        Box::new(terragrunt::UnpinnedSource),
        Box::new(terragrunt::DependencyCycle),
        Box::new(terragrunt::MissingDependency),
        Box::new(terragrunt::IncludeDepth),
        Box::new(terragrunt::MissingLocalSource),
        Box::new(github_actions::UnknownNeeds),
        Box::new(github_actions::NeedsCycle),
        Box::new(github_actions::MissingLocalReference),
        Box::new(compose::DependsCycle),
        Box::new(terragrunt::HardcodedAccount),
        Box::new(terragrunt::MissingRemoteState),
        Box::new(ci::LatestImage),
        Box::new(ci::UnpinnedInclude),
        Box::new(ci::LongScript),
        Box::new(ci::UnpinnedOrb),
        Box::new(compose::LatestImage),
        Box::new(compose::Privileged),
        Box::new(ansible::UnnamedTask),
        Box::new(ansible::NonIdempotent),
        Box::new(shell::MissingStrictMode),
        Box::new(shell::CurlPipeShell),
        Box::new(shell::UnquotedExpansion),
        Box::new(makefile::MissingPhony),
        Box::new(makefile::DuplicateTarget),
        Box::new(secrets::SecretLiteral),
        Box::new(secrets::CommittedSecretFile),
        Box::new(repo::GitignoreGaps),
        Box::new(repo::StaleCodeowners),
    ]
}

/// `# rusty-ignore: rule_a, rule_b` on the line of a finding or the one above silences it.
fn suppressed(file: &InfraFile<'_>, finding: &Finding) -> bool {
    let named = |line: u32| {
        let text = file.line(line);
        text.split_once("rusty-ignore:").is_some_and(|(_, rest)| {
            rest.split(',')
                .any(|id| id.trim() == finding.detector || id.trim() == "all")
        })
    };
    named(finding.start_line) || (finding.start_line > 1 && named(finding.start_line - 1))
}

/// Runs every enabled rule that applies; findings sorted by line, then rule.
pub fn check_file(
    file: &InfraFile<'_>,
    context: &InfraContext<'_>,
    config: &InfraConfig,
) -> Vec<Finding> {
    let mut findings: Vec<Finding> = registry()
        .iter()
        .filter(|r| r.applies(file.kind) && !config.disabled.contains(r.id()))
        .flat_map(|r| r.check(file, context, config))
        .filter(|f| !suppressed(file, f))
        .collect();
    findings.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then(a.detector.cmp(&b.detector))
            .then(a.id.cmp(&b.id))
    });
    findings
}
