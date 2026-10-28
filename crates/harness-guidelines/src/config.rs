//! The rules file as a person writes it.
use harness_code_health::DetectorConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// `.rusty/guidelines/rules.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuidelinesFile {
    pub version: u32,
    /// Limits every source file is held to; a rule of kind `metric` narrows or widens them for
    /// some files.
    #[serde(default, skip_serializing_if = "MetricLimits::is_empty")]
    pub limits: MetricLimits,
    /// Code health thresholds for this project, over the defaults: any detector's settings, and
    /// `disabled` detector ids. Code health and the goal loop measure with these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub detectors: Option<DetectorConfig>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Unknown fields are not refused here: the rule's kind (flattened in) reads the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rule {
    /// Unique, kebab-case: what violations and the tool name it by.
    pub id: String,
    #[serde(flatten)]
    pub kind: RuleKind,
    /// Globs of the files the rule applies to, relative to the repository root; empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies: Vec<String>,
    /// Globs of files the rule never applies to (tests, generated code).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub severity: Severity,
    /// What a violation says; each kind has a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// How to fix a violation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Switched off without deleting it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleKind {
    /// Text that must not appear: a regex over each line, or a tree-sitter query over the syntax
    /// (`lang` names the grammar; a capture named `violation` marks the lines, otherwise the first
    /// capture). Exactly one of `regex` and `query`.
    Pattern {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
    },
    /// Size and complexity limits for the files the rule applies to, over the file's `limits`.
    Metric {
        #[serde(flatten)]
        limits: MetricLimits,
    },
    /// Files the rule applies to must not import files matching `must_not_import`.
    Boundary { must_not_import: Vec<String> },
    /// A guideline in words, given to the model for the files it applies to. Not checked by
    /// the host.
    Prose { text: String },
}

impl RuleKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pattern { .. } => "pattern",
            Self::Metric { .. } => "metric",
            Self::Boundary { .. } => "boundary",
            Self::Prose { .. } => "prose",
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Shown to the model; never reported as a violation.
    Info,
    /// Reported, never blocks.
    Warn,
    /// Blocks finishing while the change introduced it.
    #[default]
    Error,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Unset limits do not apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricLimits {
    /// Lines in the file, blank and comment lines included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_lines: Option<u32>,
    /// Lines of code in one function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_function_lines: Option<u32>,
    /// Cyclomatic complexity of one function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_complexity: Option<u32>,
    /// Deepest nesting of control blocks in one function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nesting: Option<u32>,
    /// Declared parameters of one function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_params: Option<u32>,
}

impl MetricLimits {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `self` with every limit `other` sets replaced by it.
    pub fn over(self, other: &Self) -> Self {
        Self {
            max_file_lines: other.max_file_lines.or(self.max_file_lines),
            max_function_lines: other.max_function_lines.or(self.max_function_lines),
            max_complexity: other.max_complexity.or(self.max_complexity),
            max_nesting: other.max_nesting.or(self.max_nesting),
            max_params: other.max_params.or(self.max_params),
        }
    }
}
