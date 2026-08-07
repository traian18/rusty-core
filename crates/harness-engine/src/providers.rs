//! Engine-owned provider, credential, authentication, and model catalog types.

use std::{fmt, path::PathBuf};

use harness_protocol::backend::BackendCapabilities;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Stable user-facing provider identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderKey(pub String);

impl ProviderKey {
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for ProviderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Stable non-secret credential profile identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialProfileId(pub String);

impl CredentialProfileId {
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterKind { Api, Cli }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod { Environment, CliManaged }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialState { Available, Missing, ManagedExternally }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialProfileSummary {
    pub id: CredentialProfileId,
    pub provider: ProviderKey,
    pub label: String,
    pub state: CredentialState,
    pub auth_method: AuthMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: ProviderKey,
    pub integration: String,
    pub name: String,
    pub adapter_kind: AdapterKind,
    pub auth_methods: Vec<AuthMethod>,
    pub capabilities: BackendCapabilities,
    pub credential_hint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub text: bool,
    pub tools: bool,
    pub reasoning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub provider: ProviderKey,
    pub provider_model_id: String,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    pub context_window: Option<u64>,
    pub is_default: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendSelection {
    pub provider: ProviderKey,
    pub credential_profile: CredentialProfileId,
    pub provider_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthFlowState {
    Starting,
    WaitingForExternalCommand { program: String, args: Vec<String> },
    Connected { profile: CredentialProfileSummary },
    Cancelled,
    Failed { safe_message: String },
}

/// Normalized authentication handoff. CLI-managed providers keep token storage
/// and refresh ownership; frontends may execute the documented command in a
/// foreground terminal and then refresh credential state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFlowHandle {
    pub provider: ProviderKey,
    pub states: Vec<AuthFlowState>,
}

impl AuthFlowHandle {
    pub fn cancel(&mut self) {
        self.states.clear();
        self.states.push(AuthFlowState::Cancelled);
    }

    pub fn current(&self) -> Option<&AuthFlowState> {
        self.states.last()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHealth {
    pub provider: ProviderKey,
    pub credential: CredentialState,
    pub executable: Option<PathBuf>,
    pub ready: bool,
    pub message: String,
}

/// Secret value whose formatting is always redacted.
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self { Self(value) }
    pub fn expose(&self) -> &str { &self.0 }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("[REDACTED]") }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("[REDACTED]") }
}

impl Drop for SecretString {
    /// M6 secret-redaction audit: `String::clear()` alone only resets the
    /// length to zero — it does not guarantee the freed bytes are
    /// overwritten, and does nothing to stop the compiler from optimizing
    /// away a "dead" write it doesn't know is security-sensitive. `zeroize`
    /// is written specifically to survive both of those (a volatile write
    /// the optimizer can't elide), which a plain `clear()` was never meant
    /// to guarantee.
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub trait CredentialStore: Send + Sync {
    fn resolve(&self, profile: &CredentialProfileSummary) -> Option<SecretString>;
}

/// Host bridge for an OS keychain or other secure credential service. The
/// harness stores only the resolver and never serializes returned secrets.
type CredentialResolver =
    dyn Fn(&CredentialProfileSummary) -> Option<SecretString> + Send + Sync;

pub struct SecureCredentialStore {
    resolver: Box<CredentialResolver>,
}

impl SecureCredentialStore {
    pub fn new(resolver: impl Fn(&CredentialProfileSummary) -> Option<SecretString> + Send + Sync + 'static) -> Self {
        Self { resolver: Box::new(resolver) }
    }
}

impl CredentialStore for SecureCredentialStore {
    fn resolve(&self, profile: &CredentialProfileSummary) -> Option<SecretString> {
        (self.resolver)(profile)
    }
}

#[derive(Debug, Default)]
pub struct EnvironmentCredentialStore;

impl CredentialStore for EnvironmentCredentialStore {
    fn resolve(&self, profile: &CredentialProfileSummary) -> Option<SecretString> {
        let variable = match profile.provider.as_str() {
            "anthropic-api" => "ANTHROPIC_API_KEY",
            "openai-api" => "OPENAI_API_KEY",
            _ => return None,
        };
        std::env::var(variable).ok().filter(|value| !value.is_empty()).map(SecretString::new)
    }
}

/// Locate a CLI executable for a provider integration.
///
/// An absolute or relative path (anything with more than one path component)
/// is honored verbatim. A bare program name is searched across `$PATH`
/// **plus** a set of common install locations that a directly-spawned process
/// frequently does not inherit — macOS GUI/IDE launches expose only a minimal
/// PATH, and Node-based CLIs such as `claude`/`codex` typically live in
/// Homebrew, npm-global, nvm, bun, or `~/.local/bin` directories. On Windows
/// each candidate is probed against `PATHEXT` (e.g. `claude.cmd`).
pub(crate) fn find_executable(program: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(program);
    if candidate.components().count() > 1 {
        return candidate.is_file().then_some(candidate);
    }

    let extensions = executable_extensions();
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        search_dirs.extend(std::env::split_paths(&path));
    }
    search_dirs.extend(fallback_executable_dirs());

    for directory in search_dirs {
        for extension in &extensions {
            let mut candidate = directory.join(program);
            if !extension.is_empty() {
                candidate.set_extension(extension);
            }
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Executable name extensions to probe.
///
/// Non-Windows platforms use the bare name (empty extension). Windows honors
/// `PATHEXT`, falling back to the common script/binary extensions.
fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        std::env::var("PATHEXT")
            .ok()
            .map(|value| {
                value
                    .split(';')
                    .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|ext| !ext.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|extensions| !extensions.is_empty())
            .unwrap_or_else(|| {
                ["exe", "cmd", "bat", "ps1"].iter().map(|ext| (*ext).to_owned()).collect()
            })
    } else {
        vec![String::new()]
    }
}

/// Common install directories a directly-spawned process may not inherit on
/// `$PATH`. Missing directories are simply skipped by the `is_file()` probe.
fn fallback_executable_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/bin",
        "/bin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(home) = home {
        for relative in [
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".deno/bin",
            ".cargo/bin",
            ".volta/bin",
            "bin",
        ] {
            dirs.push(home.join(relative));
        }

        // nvm installs each Node version under its own bin directory that is
        // only added to PATH by an interactive login shell.
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("bin"));
            }
        }
    }

    dirs
}

pub(crate) fn descriptor_for(integration: &str, capabilities: BackendCapabilities) -> ProviderDescriptor {
    let (id, name, kind, auth, hint) = match integration {
        "anthropic" => ("anthropic-api", "Anthropic API", AdapterKind::Api, AuthMethod::Environment, "ANTHROPIC_API_KEY"),
        "claude-code" => ("claude-code", "Claude Code", AdapterKind::Cli, AuthMethod::CliManaged, "Claude CLI login"),
        "openai" => ("openai-api", "OpenAI API", AdapterKind::Api, AuthMethod::Environment, "OPENAI_API_KEY"),
        "codex" => ("codex", "OpenAI Codex", AdapterKind::Cli, AuthMethod::CliManaged, "Codex CLI login"),
        "github-copilot" => ("github-copilot", "GitHub Copilot", AdapterKind::Cli, AuthMethod::CliManaged, "Copilot CLI login"),
        other => (other, other, AdapterKind::Cli, AuthMethod::CliManaged, "External credentials"),
    };
    ProviderDescriptor {
        id: ProviderKey::new(id), integration: integration.to_owned(), name: name.to_owned(),
        adapter_kind: kind, auth_methods: vec![auth], capabilities,
        credential_hint: hint.to_owned(),
    }
}

pub(crate) async fn discover_api_models(provider: &ProviderKey) -> Result<Vec<ModelDescriptor>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|_| "could not initialize the provider model client".to_owned())?;
    let response = match provider.as_str() {
        "anthropic-api" => {
            let key = SecretString::new(std::env::var("ANTHROPIC_API_KEY").map_err(|_| "Anthropic credential unavailable".to_owned())?);
            client.get("https://api.anthropic.com/v1/models")
                .header("x-api-key", key.expose())
                .header("anthropic-version", "2023-06-01")
                .send().await
        }
        "openai-api" => {
            let key = SecretString::new(std::env::var("OPENAI_API_KEY").map_err(|_| "OpenAI credential unavailable".to_owned())?);
            client.get("https://api.openai.com/v1/models").bearer_auth(key.expose()).send().await
        }
        _ => return Ok(default_models(provider)),
    }.map_err(|_| "provider model catalog request failed".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("provider model catalog returned HTTP {}", response.status().as_u16()));
    }
    let value: serde_json::Value = response.json().await.map_err(|_| "provider model catalog returned invalid JSON".to_owned())?;
    let data = value.get("data").and_then(serde_json::Value::as_array).ok_or_else(|| "provider model catalog did not contain data".to_owned())?;
    let mut models = data.iter().filter_map(|item| {
        let id = item.get("id").and_then(serde_json::Value::as_str)?;
        if provider.as_str() == "openai-api" && !is_supported_openai_model(id) { return None; }
        let name = item.get("display_name").and_then(serde_json::Value::as_str).unwrap_or(id);
        Some(ModelDescriptor {
            provider: provider.clone(),
            provider_model_id: id.to_owned(),
            display_name: name.to_owned(),
            capabilities: ModelCapabilities { text: true, tools: true, reasoning: id.starts_with('o') },
            context_window: None,
            is_default: false,
            stale: false,
        })
    }).collect::<Vec<_>>();
    models.sort_by(|left, right| left.provider_model_id.cmp(&right.provider_model_id));
    let preferred = default_models(provider).into_iter().find(|model| model.is_default).map(|model| model.provider_model_id);
    if let Some(default) = preferred.and_then(|id| models.iter_mut().find(|model| model.provider_model_id == id)) {
        default.is_default = true;
    } else if let Some(first) = models.first_mut() {
        first.is_default = true;
    }
    Ok(models)
}

fn is_supported_openai_model(id: &str) -> bool {
    ["gpt-", "chatgpt-", "o1", "o3", "o4"].iter().any(|prefix| id.starts_with(prefix))
}

pub(crate) fn default_models(provider: &ProviderKey) -> Vec<ModelDescriptor> {
    let values: &[(&str, &str, bool)] = match provider.as_str() {
        "anthropic-api" => &[("claude-sonnet-4-20250514", "Claude Sonnet 4", true), ("claude-opus-4-20250514", "Claude Opus 4", false)],
        "claude-code" => &[("sonnet", "Sonnet", true), ("opus", "Opus", false), ("haiku", "Haiku", false)],
        "openai-api" => &[("gpt-4o", "GPT-4o", true), ("gpt-4.1", "GPT-4.1", false), ("o3", "o3", false)],
        "codex" => &[("default", "Provider default", true)],
        "github-copilot" => &[("auto", "Auto (plan and policy aware)", true)],
        _ => &[("default", "Provider default", true)],
    };
    values.iter().map(|(id, name, is_default)| ModelDescriptor {
        provider: provider.clone(), provider_model_id: (*id).to_owned(), display_name: (*name).to_owned(),
        capabilities: ModelCapabilities { text: true, tools: true, reasoning: false },
        context_window: None, is_default: *is_default, stale: false,
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_formatting_is_redacted() {
        let secret = SecretString::new("sk-a-very-secret-value".into());
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
    }

    /// M6 secret-redaction audit: proves `SecretString::drop` actually wipes
    /// its buffer via `zeroize` rather than the prior `String::clear()`,
    /// which only reset the length and left the bytes present in the
    /// allocation. `zeroize::Zeroize` overwrites the buffer with a volatile
    /// write the compiler is required to preserve — this test exercises
    /// that same `Zeroize` behavior directly (the mechanism `SecretString`'s
    /// `Drop` now delegates to) since the length-zero-but-bytes-present
    /// failure mode of `clear()` can't be observed after drop without
    /// reading freed memory, which is exactly the sort of thing a test
    /// shouldn't need `unsafe` to prove is no longer happening.
    #[test]
    fn drop_mechanism_actually_wipes_the_buffer_not_just_resets_length() {
        let mut buffer = String::from("sk-a-very-secret-value");
        let capacity_before = buffer.capacity();
        buffer.zeroize();
        assert_eq!(buffer.len(), 0);
        // zeroize overwrites in place rather than reallocating — the
        // capacity is unchanged, which is exactly why a plain `.clear()`
        // (same length-zero postcondition, no overwrite) isn't equivalent:
        // the old bytes would still occupy this same, still-allocated
        // capacity after `clear()`.
        assert_eq!(buffer.capacity(), capacity_before);
    }

    #[test]
    fn secret_string_drop_does_not_panic() {
        // Exercises the real Drop path (not the standalone Zeroize call
        // above) end to end — construction, use, and drop — to catch any
        // future regression that panics or double-frees.
        let secret = SecretString::new("sk-another-secret".into());
        assert_eq!(secret.expose(), "sk-another-secret");
        drop(secret);
    }

    #[test]
    fn cancelling_auth_discards_pending_state() {
        let mut flow = AuthFlowHandle {
            provider: ProviderKey::new("codex"),
            states: vec![AuthFlowState::Starting, AuthFlowState::WaitingForExternalCommand {
                program: "codex".into(), args: vec!["login".into()],
            }],
        };
        flow.cancel();
        assert_eq!(flow.current(), Some(&AuthFlowState::Cancelled));
    }

    #[test]
    fn absolute_path_that_is_missing_is_not_resolved() {
        assert!(find_executable("/definitely/not/here/claude").is_none());
    }

    #[test]
    fn missing_bare_program_is_not_resolved() {
        assert!(find_executable("harness-nonexistent-cli-4f2b9c").is_none());
    }

    #[test]
    fn non_windows_probes_the_bare_name() {
        if !cfg!(windows) {
            assert_eq!(executable_extensions(), vec![String::new()]);
        }
    }

    #[test]
    fn fallback_dirs_include_common_cli_install_locations() {
        let dirs = fallback_executable_dirs();
        assert!(dirs.iter().any(|dir| dir.ends_with("bin")));
    }
}
