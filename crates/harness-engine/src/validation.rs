//! Editor-facing validation of behavior profiles and orchestration
//! definitions as raw JSON documents.
//!
//! An editor holds documents that may not even parse yet, so these functions
//! never fail: every problem (parse errors included) comes back as an
//! [`Issue`] with a `path` the editor can attach to a field.

use harness_core::behavior::{
    self, Action, BehaviorProfile, ProfileRef, ProfileRegistry, ProfileSource,
};
use harness_core::orchestration::{self, OrchestrationDefinition, OrchestrationNodeKind};
use serde::Serialize;
use serde_json::Value;

/// One problem with a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Location inside the document in the core compilers' notation
    /// (`rules[2].do`, `nodes.<id>.config`); `""` for the whole document.
    pub path: String,
    /// Stable machine-readable code.
    pub code: String,
    pub message: String,
    /// `false` for warnings that do not stop the document from running.
    pub blocking: bool,
}

impl Issue {
    fn error(path: impl Into<String>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.into(),
            message: message.into(),
            blocking: true,
        }
    }

    fn warning(
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            blocking: false,
            ..Self::error(path, code, message)
        }
    }
}

/// Validate `document` as a behavior profile. `library` holds the other
/// profiles the editor knows about (typically the rest of the workspace);
/// `switch_profile` targets that resolve neither there nor to a built-in are
/// reported. Documents in `library` that do not parse are ignored.
pub fn validate_profile(document: &Value, library: &[Value]) -> Vec<Issue> {
    let profile = match serde_json::from_value::<BehaviorProfile>(document.clone()) {
        Ok(profile) => profile,
        Err(error) => return vec![Issue::error("", "parse", error.to_string())],
    };
    let mut issues = match behavior::compile(profile.clone()) {
        Ok(_) => Vec::new(),
        Err(error) => error
            .issues
            .into_iter()
            .map(|issue| Issue::error(issue.path, issue.code, issue.message))
            .collect(),
    };
    let registry = library_registry(library, Some(&profile));
    for (index, rule) in profile.rules.iter().enumerate() {
        if let Action::SwitchProfile { profile: target } = &rule.action {
            if target.id == profile.id {
                continue;
            }
            if registry.resolve(target).is_err() {
                issues.push(Issue::error(
                    format!("rules[{index}].do.switch_profile.profile"),
                    "unknown_profile",
                    format!("no profile {target} in this workspace"),
                ));
            }
        }
    }
    issues
}

/// Validate `document` as an orchestration definition. Agent steps that name
/// a profile are checked against `profiles` (plus the built-ins).
pub fn validate_orchestration(document: &Value, profiles: &[Value]) -> Vec<Issue> {
    let definition = match serde_json::from_value::<OrchestrationDefinition>(document.clone()) {
        Ok(definition) => definition,
        Err(error) => return vec![Issue::error("", "parse", error.to_string())],
    };
    let mut issues = match orchestration::compile(definition.clone()) {
        Ok(_) => Vec::new(),
        Err(error) => error
            .issues
            .into_iter()
            .map(|issue| Issue::error(issue.path, issue.code, issue.message))
            .collect(),
    };
    let registry = library_registry(profiles, None);
    for node in &definition.nodes {
        if let OrchestrationNodeKind::Agent(config) = &node.kind {
            if let Some(reference) = &config.profile {
                if registry.resolve(reference).is_err() {
                    issues.push(Issue::warning(
                        format!("nodes.{}.config.profile", node.id),
                        "unknown_profile",
                        format!("no profile {reference} in this workspace"),
                    ));
                }
            }
        }
    }
    issues
}

/// Profiles reachable from `document` through `switch_profile` rules, as
/// references (targets that do not resolve are left out; see
/// [`validate_profile`]).
pub fn profile_switch_targets(document: &Value) -> Vec<ProfileRef> {
    serde_json::from_value::<BehaviorProfile>(document.clone())
        .map(|profile| {
            profile
                .rules
                .into_iter()
                .filter_map(|rule| match rule.action {
                    Action::SwitchProfile { profile } => Some(profile),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Documents an editor can start from: the built-in profiles (read-only,
/// ids under `rusty.`) and the default single-agent workflow.
#[derive(Debug, Clone, Serialize)]
pub struct Templates {
    pub builtin_profiles: Vec<Value>,
    pub default_workflow: Value,
}

pub fn templates() -> Templates {
    Templates {
        builtin_profiles: vec![serde_json::to_value(behavior::default_profile_definition())
            .expect("profiles serialize")],
        default_workflow: serde_json::to_value(orchestration::default_orchestration_definition())
            .expect("definitions serialize"),
    }
}

fn library_registry(library: &[Value], current: Option<&BehaviorProfile>) -> ProfileRegistry {
    let mut registry = ProfileRegistry::new().allow_drafts(true);
    let parsed = library
        .iter()
        .filter_map(|document| serde_json::from_value::<BehaviorProfile>(document.clone()).ok())
        .chain(current.cloned());
    for profile in parsed {
        // Invalid or reserved documents are reported on their own; here
        // they simply do not resolve.
        let _ = registry.register(profile, ProfileSource::Workspace);
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(id: &str, rules: Value) -> Value {
        json!({ "schema_version": 1, "id": id, "revision": 1, "name": id, "rules": rules })
    }

    #[test]
    fn unparsable_documents_report_one_parse_issue() {
        let issues = validate_profile(&json!({ "id": "x" }), &[]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "parse");
    }

    #[test]
    fn switch_targets_must_exist_in_the_library() {
        let rules = json!([{ "id": "go", "on": "RunStart",
            "do": { "switch_profile": { "profile": { "id": "review" } } } }]);
        let document = profile("build", rules);
        let missing = validate_profile(&document, &[]);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert_eq!(missing[0].path, "rules[0].do.switch_profile.profile");
        assert!(validate_profile(&document, &[profile("review", json!([]))]).is_empty());
        assert_eq!(
            profile_switch_targets(&document),
            vec![ProfileRef::latest("review")]
        );
    }

    #[test]
    fn compiler_issues_are_passed_through() {
        let rules = json!([{ "id": "bad", "on": "RunStart",
            "do": { "inject": { "text": "x", "placement": "with_result" } } }]);
        let issues = validate_profile(&profile("p", rules), &[]);
        assert!(!issues.is_empty());
        assert!(issues.iter().all(|issue| issue.blocking));
    }

    #[test]
    fn default_orchestration_validates_and_unknown_profiles_warn() {
        let mut definition =
            serde_json::to_value(orchestration::default_orchestration_definition()).unwrap();
        assert!(validate_orchestration(&definition, &[]).is_empty());
        let nodes = definition["nodes"].as_array_mut().unwrap();
        let agent = nodes
            .iter_mut()
            .find(|node| node["type"] == "agent")
            .unwrap();
        agent["config"]["profile"] = json!({ "id": "missing" });
        let issues = validate_orchestration(&definition, &[]);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(!issues[0].blocking);
    }
}
