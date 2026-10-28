//! One file through every rule that applies to it.
use harness_code_analysis::metrics::FileMetrics;
use serde::Serialize;

use crate::{
    boundary::{crossings, Import},
    compile::Guidelines,
    config::{RuleKind, Severity},
    metric::excesses,
};

/// The id violations of the file-wide `limits` carry.
pub const LIMITS_RULE: &str = "limits";

/// What the host knows about one file.
pub struct FileInput<'a> {
    /// Repository-relative, `/`-separated.
    pub path: &'a str,
    pub source: &'a str,
    /// `None` when the language is not measured.
    pub metrics: Option<&'a FileMetrics>,
    /// The file's resolved imports; `None` when they are not known (no index yet), which skips
    /// boundary rules.
    pub imports: Option<&'a [Import]>,
}

/// Which kinds of rules to run: the fast ones after every edit, all of them otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kinds {
    pub pattern: bool,
    pub metric: bool,
    pub boundary: bool,
}

impl Kinds {
    pub const ALL: Self = Self {
        pattern: true,
        metric: true,
        boundary: true,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub rule: String,
    pub kind: &'static str,
    pub severity: Severity,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    /// The function, for limits on one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// For a file-wide limit: what the file measured, so a change can be judged by whether it
    /// made it worse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<u32>,
}

impl Violation {
    pub fn file_wide(&self) -> bool {
        self.kind == "metric" && self.function.is_none()
    }
}

fn line_count(source: &str) -> u32 {
    source.lines().count() as u32
}

/// Every violation in `file`, in rule order, then by line. `info` rules report nothing.
pub fn check_file(guidelines: &Guidelines, file: &FileInput<'_>, kinds: Kinds) -> Vec<Violation> {
    let mut out = Vec::new();
    if kinds.metric {
        out.extend(limit_violations(guidelines, file));
    }
    for compiled in guidelines.rules.iter().filter(|c| c.applies_to(file.path)) {
        let rule = &compiled.rule;
        if rule.severity == Severity::Info {
            continue;
        }
        let violation = |start_line: u32, end_line: u32, message: String| Violation {
            rule: rule.id.clone(),
            kind: rule.kind.name(),
            severity: rule.severity,
            path: file.path.to_owned(),
            start_line,
            end_line,
            function: None,
            message: rule.message.clone().unwrap_or(message),
            fix: rule.fix.clone(),
            measured: None,
        };
        match &rule.kind {
            RuleKind::Pattern { .. } if kinds.pattern => {
                let pattern = compiled.pattern.as_ref().expect("compiled with the rule");
                for hit in pattern.hits(file.path, file.source) {
                    out.push(violation(
                        hit.start_line,
                        hit.end_line,
                        format!("matches the '{}' pattern", rule.id),
                    ));
                }
            }
            RuleKind::Boundary { .. } if kinds.boundary => {
                for import in crossings(&compiled.forbidden, file.imports.unwrap_or_default()) {
                    out.push(violation(
                        import.line,
                        import.line,
                        format!(
                            "imports {}, which this part of the code must not import",
                            import.target
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

/// The file's limits, with the metric rules that apply folded in. A limit set only by a
/// `warn` rule reports as a warning.
fn limit_violations(guidelines: &Guidelines, file: &FileInput<'_>) -> Vec<Violation> {
    let limits = guidelines.limits_for(file.path);
    let severity = guidelines
        .rules
        .iter()
        .filter(|c| c.applies_to(file.path) && matches!(c.rule.kind, RuleKind::Metric { .. }))
        .map(|c| c.rule.severity)
        .max()
        .unwrap_or(Severity::Error);
    if severity == Severity::Info {
        return Vec::new();
    }
    excesses(&limits, line_count(file.source), file.metrics)
        .into_iter()
        .map(|excess| {
            let subject = excess
                .function
                .as_deref()
                .map_or_else(|| "the file".to_owned(), |f| format!("'{f}'"));
            let fix = if excess.function.is_some() {
                "Extract parts of it into well-named functions."
            } else {
                "Split the file by responsibility into smaller modules."
            };
            Violation {
                rule: LIMITS_RULE.into(),
                kind: "metric",
                severity,
                path: file.path.to_owned(),
                start_line: excess.start_line,
                end_line: excess.end_line,
                message: format!(
                    "{subject} has {} {} (the limit is {})",
                    excess.value, excess.what, excess.limit
                ),
                measured: excess.function.is_none().then_some(excess.value),
                function: excess.function,
                fix: Some(fix.into()),
            }
        })
        .collect()
}
