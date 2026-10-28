//! The rules file parsed and validated, with globs and patterns compiled once.
use std::collections::BTreeSet;

use harness_code_health::DetectorConfig;
use serde::Serialize;

use crate::{
    config::{GuidelinesFile, MetricLimits, Rule, RuleKind, SCHEMA_VERSION},
    glob::Glob,
    pattern::{self, Pattern},
    style::StyleGuide,
};

/// What is wrong with the rules file, by rule where it is about one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule {
            Some(rule) => write!(f, "rule '{rule}': {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

pub(crate) struct Compiled {
    pub rule: Rule,
    pub applies: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub pattern: Option<Pattern>,
    pub forbidden: Vec<Glob>,
}

impl Compiled {
    pub fn applies_to(&self, path: &str) -> bool {
        !self.rule.disabled
            && (self.applies.is_empty() || self.applies.iter().any(|g| g.matches(path)))
            && !self.exclude.iter().any(|g| g.matches(path))
    }
}

/// A repository's guidelines, ready to check files with.
pub struct Guidelines {
    pub(crate) limits: MetricLimits,
    pub(crate) detectors: Option<DetectorConfig>,
    pub(crate) rules: Vec<Compiled>,
    pub(crate) style: StyleGuide,
}

impl Guidelines {
    /// No rules at all: what a repository without a guidelines folder has.
    pub fn empty() -> Self {
        Self {
            limits: MetricLimits::default(),
            detectors: None,
            rules: Vec::new(),
            style: StyleGuide::default(),
        }
    }

    /// Parses `rules.json` (when present) and `style.md` (when present). Every problem is
    /// reported, not only the first.
    pub fn parse(
        rules_json: Option<&str>,
        style_md: Option<&str>,
    ) -> Result<Self, Vec<ConfigError>> {
        let style = style_md.map(StyleGuide::parse).unwrap_or_default();
        let Some(text) = rules_json else {
            return Ok(Self {
                style,
                ..Self::empty()
            });
        };
        let file: GuidelinesFile = serde_json::from_str(text).map_err(|e| {
            vec![ConfigError {
                rule: None,
                message: format!("rules.json is not valid: {e}"),
            }]
        })?;
        Self::from_file(file, style)
    }

    pub fn from_file(file: GuidelinesFile, style: StyleGuide) -> Result<Self, Vec<ConfigError>> {
        let mut errors = Vec::new();
        if file.version != SCHEMA_VERSION {
            errors.push(ConfigError {
                rule: None,
                message: format!("version must be {SCHEMA_VERSION}, found {}", file.version),
            });
        }
        let mut seen = BTreeSet::new();
        let mut rules = Vec::new();
        for rule in file.rules {
            let mut fail = |message: String| {
                errors.push(ConfigError {
                    rule: Some(rule.id.clone()),
                    message,
                })
            };
            if rule.id.trim().is_empty() {
                fail("the id is empty".into());
            } else if !seen.insert(rule.id.clone()) {
                fail("the id is used by another rule".into());
            }
            let applies = globs(&rule.applies, &mut fail);
            let exclude = globs(&rule.exclude, &mut fail);
            let mut compiled_pattern = None;
            let mut forbidden = Vec::new();
            match &rule.kind {
                RuleKind::Pattern { regex, query, lang } => {
                    match pattern::compile(regex.as_deref(), query.as_deref(), lang.as_deref()) {
                        Ok(p) => compiled_pattern = Some(p),
                        Err(message) => fail(message),
                    }
                }
                RuleKind::Boundary { must_not_import } => {
                    if must_not_import.is_empty() {
                        fail("must_not_import is empty".into());
                    }
                    if rule.applies.is_empty() {
                        fail("a boundary needs 'applies': the files it constrains".into());
                    }
                    forbidden = globs(must_not_import, &mut fail);
                }
                RuleKind::Metric { limits } if limits.is_empty() => {
                    fail("a metric rule sets no limit".into())
                }
                RuleKind::Prose { text } if text.trim().is_empty() => {
                    fail("the text is empty".into())
                }
                RuleKind::Metric { .. } | RuleKind::Prose { .. } => {}
            }
            rules.push(Compiled {
                rule,
                applies,
                exclude,
                pattern: compiled_pattern,
                forbidden,
            });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            limits: file.limits,
            detectors: file.detectors,
            rules,
            style,
        })
    }

    /// Code health thresholds for this repository: the defaults with the file's overrides.
    pub fn detectors(&self) -> DetectorConfig {
        self.detectors.clone().unwrap_or_default()
    }

    /// Whether the repository sets any guideline at all.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.limits.is_empty() && self.style.is_empty()
    }

    /// Every rule that is switched on, in the file's order.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules
            .iter()
            .filter(|c| !c.rule.disabled)
            .map(|c| &c.rule)
    }

    /// The rules that apply to `path`, in the file's order.
    pub fn rules_for(&self, path: &str) -> Vec<&Rule> {
        self.rules
            .iter()
            .filter(|c| c.applies_to(path))
            .map(|c| &c.rule)
            .collect()
    }

    /// The limits `path` is held to: the file's limits with every metric rule that applies.
    pub fn limits_for(&self, path: &str) -> MetricLimits {
        self.rules
            .iter()
            .filter(|c| c.applies_to(path))
            .fold(self.limits, |limits, c| match &c.rule.kind {
                RuleKind::Metric { limits: own } => limits.over(own),
                _ => limits,
            })
    }

    pub fn style(&self) -> &StyleGuide {
        &self.style
    }
}

fn globs(patterns: &[String], fail: &mut impl FnMut(String)) -> Vec<Glob> {
    patterns
        .iter()
        .filter_map(|p| {
            Glob::new(p)
                .map_err(|e| fail(format!("glob '{p}': {e}")))
                .ok()
        })
        .collect()
}
