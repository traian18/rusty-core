//! One measurement of a goal's scope, taken by the host after a pass.
use serde::{Deserialize, Serialize};

use crate::contract::Targets;

/// Code health severities, mildest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingSummary {
    pub id: String,
    pub issue: String,
    pub severity: Severity,
    pub lines: String,
    pub reason: String,
    /// What fixing it is worth to the file's score.
    pub worth: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMeasure {
    pub path: String,
    /// Code health 1-10; `None` when the language is not measured.
    pub score: Option<f64>,
    /// Highest worth first.
    #[serde(default)]
    pub findings: Vec<FindingSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidelineSummary {
    pub rule: String,
    pub path: String,
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub name: String,
    pub passed: bool,
    /// The check ran after the pass's last edit; one that did not proves nothing either way.
    #[serde(default = "ran_by_default")]
    pub ran: bool,
    /// The first lines of a failure, or what ran.
    #[serde(default)]
    pub summary: String,
}

const fn ran_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub files: Vec<FileMeasure>,
    #[serde(default)]
    pub guideline_errors: Vec<GuidelineSummary>,
    #[serde(default)]
    pub guideline_warnings: u32,
    #[serde(default)]
    pub checks: Vec<CheckOutcome>,
    /// Content hash of the scope's files: equal fingerprints mean nothing changed.
    #[serde(default)]
    pub fingerprint: String,
}

/// A target the measurement does not meet yet, in words for the user and the next pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unmet {
    pub target: String,
    pub detail: String,
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

impl Measurement {
    pub fn scored(&self) -> impl Iterator<Item = (&FileMeasure, f64)> {
        self.files.iter().filter_map(|f| f.score.map(|s| (f, s)))
    }

    /// The lowest file score in scope.
    pub fn worst_score(&self) -> Option<f64> {
        self.scored().map(|(_, s)| s).min_by(f64::total_cmp)
    }

    /// The mean file score in scope.
    pub fn mean_score(&self) -> Option<f64> {
        let scores: Vec<f64> = self.scored().map(|(_, s)| s).collect();
        (!scores.is_empty()).then(|| scores.iter().sum::<f64>() / scores.len() as f64)
    }

    pub fn failing_checks(&self) -> impl Iterator<Item = &CheckOutcome> {
        self.checks.iter().filter(|c| !c.passed)
    }

    /// Every target `targets` sets that this measurement misses.
    pub fn unmet(&self, targets: &Targets) -> Vec<Unmet> {
        let mut out = Vec::new();
        if let Some(min) = targets.min_file_health {
            let below: Vec<String> = self
                .scored()
                .filter(|(_, s)| *s < min)
                .map(|(f, s)| format!("{} {}", f.path, round(s)))
                .collect();
            if !below.is_empty() {
                out.push(Unmet {
                    target: format!("every file scores at least {min}"),
                    detail: below.join(", "),
                });
            }
        }
        if let Some(max) = targets.max_severity {
            let worse: Vec<String> = self
                .files
                .iter()
                .flat_map(|f| f.findings.iter().map(move |x| (f, x)))
                .filter(|(_, x)| x.severity > max)
                .map(|(f, x)| format!("{} {} ({})", f.path, x.issue, x.lines))
                .collect();
            if !worse.is_empty() {
                out.push(Unmet {
                    target: format!("no finding above {max:?}").to_lowercase(),
                    detail: worse.join(", "),
                });
            }
        }
        for name in &targets.checks {
            match self.checks.iter().find(|c| &c.name == name) {
                Some(check) if check.passed => {}
                Some(check) if !check.ran => out.push(Unmet {
                    target: format!("{name} passes"),
                    detail: "it was not run after the last edit".into(),
                }),
                Some(check) => out.push(Unmet {
                    target: format!("{name} passes"),
                    detail: check.summary.clone(),
                }),
                None => out.push(Unmet {
                    target: format!("{name} passes"),
                    detail: "it did not run".into(),
                }),
            }
        }
        if targets.guidelines && !self.guideline_errors.is_empty() {
            let listed: Vec<String> = self
                .guideline_errors
                .iter()
                .map(|e| format!("{}:{} {}", e.path, e.line, e.rule))
                .collect();
            out.push(Unmet {
                target: "no guideline errors".into(),
                detail: listed.join(", "),
            });
        }
        out
    }

    /// One line for the journal and the user: scores, findings, guideline errors, checks.
    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        if let (Some(worst), Some(mean)) = (self.worst_score(), self.mean_score()) {
            parts.push(format!(
                "health worst {} / mean {}",
                round(worst),
                round(mean)
            ));
        }
        let findings: usize = self.files.iter().map(|f| f.findings.len()).sum();
        parts.push(format!("{findings} finding(s)"));
        parts.push(format!(
            "{} guideline error(s)",
            self.guideline_errors.len()
        ));
        if !self.checks.is_empty() {
            let failing: Vec<&str> = self.failing_checks().map(|c| c.name.as_str()).collect();
            parts.push(if failing.is_empty() {
                "checks pass".into()
            } else {
                format!("failing: {}", failing.join(", "))
            });
        }
        parts.join(", ")
    }
}
