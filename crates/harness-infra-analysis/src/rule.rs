use std::collections::{BTreeMap, BTreeSet};

use harness_code_health::{Category, Finding, Severity};

use crate::{file::InfraFile, kind::InfraKind};

/// One check on one kind of infrastructure file; the counterpart of a code-health `Detector`.
pub trait InfraRule: Send + Sync {
    /// snake_case with the pack as prefix (`tf_`, `gha_`, `df_`): findings and configuration
    /// refer to it.
    fn id(&self) -> &'static str;
    /// Bumped when what the rule reports changes.
    fn version(&self) -> u32;
    fn category(&self) -> Category;
    /// What it looks for, in a sentence.
    fn summary(&self) -> &'static str;
    fn applies(&self, kind: InfraKind) -> bool;
    fn check(
        &self,
        file: &InfraFile<'_>,
        context: &InfraContext<'_>,
        config: &InfraConfig,
    ) -> Vec<Finding>;
}

/// What is known about the repository around the file.
#[derive(Clone, Copy, Debug, Default)]
pub struct InfraContext<'a> {
    /// Repository-relative paths of every file, when known.
    pub repo_files: Option<&'a [String]>,
    /// Relations between files, when the caller built them.
    pub graph: Option<&'a crate::graph::InfraGraph>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InfraConfig {
    pub disabled: BTreeSet<String>,
    /// Thresholds by `rule_id.name`, overriding each rule's default.
    pub limits: BTreeMap<String, u32>,
}

impl InfraConfig {
    pub fn limit(&self, rule: &str, name: &str, default: u32) -> u32 {
        self.limits
            .get(&format!("{rule}.{name}"))
            .copied()
            .unwrap_or(default)
    }
}

/// A finding at `line` of `file`; `name` tells apart several findings on one line.
pub fn finding(
    rule: &dyn InfraRule,
    file: &InfraFile<'_>,
    severity: Severity,
    lines: (u32, u32),
    name: &str,
    reason: String,
) -> Finding {
    Finding {
        id: format!("{}:{}:{}:{}", rule.id(), file.path, lines.0, name),
        detector: rule.id().to_owned(),
        detector_version: rule.version(),
        category: rule.category(),
        severity,
        path: file.path.to_owned(),
        function: None,
        class: (!name.is_empty()).then(|| name.to_owned()),
        start_line: lines.0,
        end_line: lines.1.max(lines.0),
        measured: BTreeMap::new(),
        reason,
    }
}

/// `name` in backticks, as reasons write it.
pub fn code(name: &str) -> String {
    format!("`{name}`")
}

/// Declares a rule whose whole behaviour is one `check` body.
///
/// `rule!(Name, "id", Category, "summary", [Kind, ...], |this, file, config| { ... })`
macro_rules! rule {
    ($name:ident, $id:literal, $category:expr, $summary:literal, [$($kind:ident),+],
     |$this:ident, $file:ident, $config:ident| $body:block) => {
        pub struct $name;

        impl $crate::rule::InfraRule for $name {
            fn id(&self) -> &'static str {
                $id
            }
            fn version(&self) -> u32 {
                1
            }
            fn category(&self) -> harness_code_health::Category {
                $category
            }
            fn summary(&self) -> &'static str {
                $summary
            }
            fn applies(&self, kind: $crate::kind::InfraKind) -> bool {
                matches!(kind, $($crate::kind::InfraKind::$kind)|+)
            }
            fn check(
                &self,
                $file: &$crate::file::InfraFile<'_>,
                _: &$crate::rule::InfraContext<'_>,
                $config: &$crate::rule::InfraConfig,
            ) -> Vec<harness_code_health::Finding> {
                let $this = self;
                $body
            }
        }
    };
}
pub(crate) use rule;

/// Like `rule!`, with the repository context bound as the fourth closure parameter.
macro_rules! graph_rule {
    ($name:ident, $id:literal, $category:expr, $summary:literal, [$($kind:ident),+],
     |$this:ident, $file:ident, $config:ident, $context:ident| $body:block) => {
        pub struct $name;

        impl $crate::rule::InfraRule for $name {
            fn id(&self) -> &'static str {
                $id
            }
            fn version(&self) -> u32 {
                1
            }
            fn category(&self) -> harness_code_health::Category {
                $category
            }
            fn summary(&self) -> &'static str {
                $summary
            }
            fn applies(&self, kind: $crate::kind::InfraKind) -> bool {
                matches!(kind, $($crate::kind::InfraKind::$kind)|+)
            }
            fn check(
                &self,
                $file: &$crate::file::InfraFile<'_>,
                $context: &$crate::rule::InfraContext<'_>,
                $config: &$crate::rule::InfraConfig,
            ) -> Vec<harness_code_health::Finding> {
                let $this = self;
                $body
            }
        }
    };
}
pub(crate) use graph_rule;
