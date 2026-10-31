//! Turning a profile document into a [`CompiledProfile`], or reporting everything wrong with it.
//!
//! [`compile`] runs one group of checks after another and collects every issue instead of
//! stopping at the first, so an editor can show them all at once. Each group lives in its own
//! module: `profile_checks` (identity, instructions, tools, limits), `rule_checks` (rules and
//! their actions), `condition_checks`, and `gate_checks` (the completion gate and its evaluators).

mod compiled;
mod condition_checks;
mod gate_checks;
mod profile_checks;
mod rule_checks;

use super::definition::BehaviorProfile;

/// A validated profile plus its content hash. Immutable; shared by `Arc`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledProfile {
    pub profile: BehaviorProfile,
    pub content_hash: String,
}

/// One problem in a profile document, located by a path such as `rules[2].when`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIssue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileValidationError {
    pub issues: Vec<ProfileIssue>,
}

impl std::fmt::Display for ProfileValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid behavior profile")?;
        for issue in &self.issues {
            write!(formatter, "; {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ProfileValidationError {}

/// The issues found so far.
#[derive(Default)]
pub(super) struct Issues(Vec<ProfileIssue>);

impl Issues {
    fn add(&mut self, path: &str, code: &'static str, message: impl Into<String>) {
        self.0.push(ProfileIssue {
            path: path.to_owned(),
            code,
            message: message.into(),
        });
    }

    /// Reports `message` when `text` is empty or blank.
    fn require_text(&mut self, path: &str, text: &str, message: &str) {
        if text.trim().is_empty() {
            self.add(path, "empty_text", message);
        }
    }
}

/// Validate a profile, reporting every issue found rather than the first.
pub fn compile(profile: BehaviorProfile) -> Result<CompiledProfile, ProfileValidationError> {
    let mut issues = Issues::default();
    profile_checks::check_identity(&profile, &mut issues);
    profile_checks::check_instructions(&profile, &mut issues);
    profile_checks::check_tools(&profile, &mut issues);
    profile_checks::check_execution_and_limits(&profile, &mut issues);
    rule_checks::check_rules_and_children(&profile, &mut issues);

    if !issues.0.is_empty() {
        return Err(ProfileValidationError { issues: issues.0 });
    }
    let content_hash = crate::content_hash::content_hash(&profile);
    Ok(CompiledProfile {
        profile,
        content_hash,
    })
}
