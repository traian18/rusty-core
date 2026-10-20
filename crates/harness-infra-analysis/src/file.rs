//! One infrastructure file, parsed once for the syntax its kind is written in.
use crate::{
    doc::{dockerfile, hcl, shell, yaml},
    kind::{classify, InfraKind},
};

pub struct InfraFile<'a> {
    pub path: &'a str,
    pub kind: InfraKind,
    pub text: &'a str,
    pub hcl: Option<hcl::Hcl>,
    pub yaml: Option<yaml::Yaml>,
    pub dockerfile: Option<dockerfile::Dockerfile>,
    pub shell: Option<tree_sitter::Tree>,
    /// Repository-relative paths of every file, when known; empty otherwise.
    pub repo_files: &'a [String],
}

impl<'a> InfraFile<'a> {
    /// `None` when the path is not an infrastructure file.
    pub fn new(path: &'a str, text: &'a str) -> Option<Self> {
        Some(Self::of_kind(path, text, classify(path, text)?))
    }

    pub fn of_kind(path: &'a str, text: &'a str, kind: InfraKind) -> Self {
        let hcl =
            matches!(kind, InfraKind::Terraform | InfraKind::Terragrunt).then(|| hcl::parse(text));
        let yaml = kind.is_yaml().then(|| yaml::parse(text));
        let dockerfile = (kind == InfraKind::Dockerfile).then(|| dockerfile::parse(text));
        let shell = (kind == InfraKind::Shell)
            .then(|| shell::parse(text))
            .flatten();
        Self {
            path,
            kind,
            text,
            hcl,
            yaml,
            dockerfile,
            shell,
            repo_files: &[],
        }
    }

    pub fn with_repo_files(mut self, files: &'a [String]) -> Self {
        self.repo_files = files;
        self
    }

    /// The text of 1-based `line`, or empty.
    pub fn line(&self, line: u32) -> &str {
        self.text
            .lines()
            .nth((line as usize).saturating_sub(1))
            .unwrap_or("")
    }

    pub fn line_count(&self) -> u32 {
        self.text.lines().count() as u32
    }

    /// Whether the parse was incomplete.
    pub fn partial(&self) -> bool {
        self.hcl.as_ref().is_some_and(|h| h.partial)
            || self.yaml.as_ref().is_some_and(|y| y.partial)
    }
}
