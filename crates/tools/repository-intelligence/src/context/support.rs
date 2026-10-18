//! The `support` block: what could be read for this target, and what could not.
use harness_workspace::{
    import_graph::{ImportCapabilities, LanguageProfile},
    repository_intelligence::known_unsupported_languages,
};
use serde_json::{json, Value};

/// Names of the supported languages, for answers about a language that is not supported.
pub(super) fn supported_languages(capabilities: &ImportCapabilities) -> Vec<String> {
    let mut names: Vec<String> = capabilities
        .languages
        .iter()
        .flat_map(|language| language.profiles.iter().map(profile_name))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// A profile's name as people write it: `rust`, `typescript`, `csharp`, `cpp`.
pub(super) fn profile_name(profile: &LanguageProfile) -> String {
    format!("{profile:?}").to_lowercase()
}

/// Documented gaps of the adapter that reads `profile`.
pub(super) fn gaps_for(capabilities: &ImportCapabilities, profile: LanguageProfile) -> Vec<String> {
    capabilities
        .languages
        .iter()
        .filter(|language| language.profiles.contains(&profile))
        .flat_map(|language| language.unsupported.clone())
        .collect()
}

/// What a tool is told when it asks about something Rusty cannot read.
pub(super) fn unsupported_block(
    capabilities: &ImportCapabilities,
    language: Option<&str>,
) -> Value {
    json!({
        "imports": "unsupported",
        "language": language,
        "supported_languages": supported_languages(capabilities),
        "known_unsupported": known_unsupported_languages().iter().map(|(_, name)| *name).collect::<std::collections::BTreeSet<_>>(),
    })
}
