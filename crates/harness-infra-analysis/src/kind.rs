//! Which kind of infrastructure file a path is: by name and location first, then by what the
//! file starts with for the YAML families that share extensions.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfraKind {
    Terraform,
    Terragrunt,
    GithubActions,
    GitlabCi,
    CircleCi,
    AzurePipelines,
    Dockerfile,
    Compose,
    Kubernetes,
    Helm,
    Ansible,
    CloudFormation,
    Shell,
    Makefile,
    RepoConfig,
}

impl InfraKind {
    /// The name the Code Health page shows as the language.
    pub fn name(self) -> &'static str {
        match self {
            Self::Terraform => "terraform",
            Self::Terragrunt => "terragrunt",
            Self::GithubActions => "github_actions",
            Self::GitlabCi => "gitlab_ci",
            Self::CircleCi => "circleci",
            Self::AzurePipelines => "azure_pipelines",
            Self::Dockerfile => "dockerfile",
            Self::Compose => "compose",
            Self::Kubernetes => "kubernetes",
            Self::Helm => "helm",
            Self::Ansible => "ansible",
            Self::CloudFormation => "cloudformation",
            Self::Shell => "shell",
            Self::Makefile => "makefile",
            Self::RepoConfig => "repo_config",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        ALL.into_iter().find(|k| k.name() == name)
    }

    /// Kinds written in YAML.
    pub fn is_yaml(self) -> bool {
        matches!(
            self,
            Self::GithubActions
                | Self::GitlabCi
                | Self::CircleCi
                | Self::AzurePipelines
                | Self::Compose
                | Self::Kubernetes
                | Self::Helm
                | Self::Ansible
                | Self::CloudFormation
        )
    }
}

pub const ALL: [InfraKind; 15] = [
    InfraKind::Terraform,
    InfraKind::Terragrunt,
    InfraKind::GithubActions,
    InfraKind::GitlabCi,
    InfraKind::CircleCi,
    InfraKind::AzurePipelines,
    InfraKind::Dockerfile,
    InfraKind::Compose,
    InfraKind::Kubernetes,
    InfraKind::Helm,
    InfraKind::Ansible,
    InfraKind::CloudFormation,
    InfraKind::Shell,
    InfraKind::Makefile,
    InfraKind::RepoConfig,
];

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn in_dir(path: &str, dir: &str) -> bool {
    path.starts_with(&format!("{dir}/")) || path.contains(&format!("/{dir}/"))
}

fn is_yaml_name(name: &str) -> bool {
    name.ends_with(".yml") || name.ends_with(".yaml")
}

/// Whether the name alone makes this a candidate; false for paths that need a look inside.
pub fn is_candidate(path: &str) -> bool {
    classify(path, "").is_some() || maybe_yaml_family(path)
}

/// A YAML file whose family is decided by its content.
pub fn maybe_yaml_family(path: &str) -> bool {
    is_yaml_name(file_name(path)) && !path.contains("/node_modules/")
}

/// The kind of `path`, using the start of its `text` to tell YAML families apart.
pub fn classify(path: &str, text: &str) -> Option<InfraKind> {
    let name = file_name(path);
    let lower = name.to_ascii_lowercase();
    if lower == "dockerfile"
        || lower.starts_with("dockerfile.")
        || lower.ends_with(".dockerfile")
        || lower == "containerfile"
    {
        return Some(InfraKind::Dockerfile);
    }
    if lower.ends_with(".tf") {
        return Some(InfraKind::Terraform);
    }
    if lower == "terragrunt.hcl"
        || lower == "root.hcl"
        || lower.ends_with(".terragrunt.hcl")
        || lower == "terragrunt.stack.hcl"
    {
        return Some(InfraKind::Terragrunt);
    }
    if lower == "makefile" || lower == "gnumakefile" || lower.ends_with(".mk") {
        return Some(InfraKind::Makefile);
    }
    if lower.ends_with(".sh") || lower.ends_with(".bash") {
        return Some(InfraKind::Shell);
    }
    let secret_file = lower.ends_with(".tfstate")
        || lower.ends_with(".tfstate.backup")
        || lower == ".env"
        || (lower.starts_with(".env.")
            && !["example", "sample", "template", "dist"]
                .iter()
                .any(|s| lower.ends_with(s)))
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower == "id_rsa"
        || lower == "id_ed25519";
    if secret_file
        || lower == ".gitignore"
        || lower == "codeowners"
        || lower == "dependabot.yml"
        || lower == "dependabot.yaml"
        || lower == "renovate.json"
        || lower == ".gitattributes"
    {
        return Some(InfraKind::RepoConfig);
    }
    if !is_yaml_name(&lower) {
        return text.starts_with("#!").then(|| shebang_kind(text)).flatten();
    }
    if in_dir(path, ".github/workflows")
        || (in_dir(path, ".github/actions") && lower.starts_with("action."))
    {
        return Some(InfraKind::GithubActions);
    }
    if lower == ".gitlab-ci.yml" || lower == ".gitlab-ci.yaml" || lower.ends_with(".gitlab-ci.yml")
    {
        return Some(InfraKind::GitlabCi);
    }
    if in_dir(path, ".circleci") {
        return Some(InfraKind::CircleCi);
    }
    if lower.starts_with("azure-pipelines") {
        return Some(InfraKind::AzurePipelines);
    }
    if lower.starts_with("docker-compose") || lower.starts_with("compose.") {
        return Some(InfraKind::Compose);
    }
    if lower == "chart.yaml" || lower == "chart.yml" {
        return Some(InfraKind::Helm);
    }
    let head = &text[..text.len().min(4096)];
    if head.contains("AWSTemplateFormatVersion") || head.contains("Type: AWS::") {
        return Some(InfraKind::CloudFormation);
    }
    if (head.contains("\napiVersion:") || head.starts_with("apiVersion:")) && head.contains("kind:")
    {
        return Some(InfraKind::Kubernetes);
    }
    if in_dir(path, "playbooks")
        || (in_dir(path, "roles") && in_dir(path, "tasks"))
        || head.contains("- hosts:")
    {
        return Some(InfraKind::Ansible);
    }
    None
}

fn shebang_kind(text: &str) -> Option<InfraKind> {
    let first = text.lines().next()?;
    ["/sh", "/bash", " sh", " bash", "/zsh"]
        .iter()
        .any(|s| first.ends_with(s))
        .then_some(InfraKind::Shell)
}
