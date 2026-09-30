//! Behavior profiles at the engine level: the host registry, workspace
//! profile loading, and resolution of the profile a session starts with.
//!
//! Every session runs under a profile. Unless one is chosen it is the
//! built-in, behavior-neutral `rusty.default`, so a host that never touches
//! profiles sees no change.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use harness_core::behavior::{
    BehaviorProfile, CompiledProfile, ProfileRef, ProfileRegistry, ProfileSource,
};
use serde::Deserialize;
use serde_json::Value;

use crate::HarnessError;

/// Workspace directory holding profile documents, relative to the root.
pub const WORKSPACE_PROFILES_DIR: &str = ".rusty/profiles";
/// Optional file in [`WORKSPACE_PROFILES_DIR`] naming the workspace default.
pub const WORKSPACE_PROFILES_CONFIG: &str = "config.json";

/// The host's profile registry. Cheap to clone; clones share it, so a
/// profile registered from an editor is visible to every session started
/// afterwards.
#[derive(Clone, Default)]
pub struct ProfilesConfig {
    registry: Arc<RwLock<ProfileRegistry>>,
}

impl ProfilesConfig {
    pub fn new(registry: ProfileRegistry) -> Self {
        Self {
            registry: Arc::new(RwLock::new(registry)),
        }
    }

    /// Validate and register a profile from the host (highest precedence).
    /// Published revisions are immutable; drafts may be replaced.
    pub fn register(&self, profile: BehaviorProfile) -> Result<ProfileRef, HarnessError> {
        self.registry
            .write()
            .expect("profile registry lock poisoned")
            .register(profile, ProfileSource::Host)
            .map(|compiled| compiled.reference())
            .map_err(|error| HarnessError::Profile(error.to_string()))
    }

    /// Validate and register a profile given as JSON, as an editor sends it.
    pub fn register_json(&self, profile: Value) -> Result<ProfileRef, HarnessError> {
        let profile: BehaviorProfile = serde_json::from_value(profile)
            .map_err(|error| HarnessError::Profile(error.to_string()))?;
        self.register(profile)
    }

    /// A point-in-time copy of the registry.
    pub fn snapshot(&self) -> ProfileRegistry {
        self.registry
            .read()
            .expect("profile registry lock poisoned")
            .clone()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceProfilesConfig {
    #[serde(default)]
    default: Option<ProfileRef>,
}

/// Load `<root>/.rusty/profiles/*.json` into `registry` as workspace
/// profiles. Each file holds one profile document or an array of them;
/// `config.json` may name the workspace default. A missing directory is not
/// an error. An invalid document is: a workspace that ships a broken
/// profile should hear about it, not run silently under another one.
pub fn load_workspace_profiles(
    root: &Path,
    registry: &mut ProfileRegistry,
) -> Result<(), HarnessError> {
    let dir = root.join(WORKSPACE_PROFILES_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(profile_file_error(&dir, error)),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();

    for path in files {
        let contents =
            std::fs::read_to_string(&path).map_err(|error| profile_file_error(&path, error))?;
        if path
            .file_name()
            .is_some_and(|name| name == WORKSPACE_PROFILES_CONFIG)
        {
            let config: WorkspaceProfilesConfig = serde_json::from_str(&contents)
                .map_err(|error| profile_file_error(&path, error))?;
            registry.set_default(config.default);
            continue;
        }
        let documents = match serde_json::from_str::<Value>(&contents)
            .map_err(|error| profile_file_error(&path, error))?
        {
            Value::Array(documents) => documents,
            document => vec![document],
        };
        for document in documents {
            let profile: BehaviorProfile = serde_json::from_value(document)
                .map_err(|error| profile_file_error(&path, error))?;
            registry
                .register(profile, ProfileSource::Workspace)
                .map_err(|error| profile_file_error(&path, error))?;
        }
    }
    Ok(())
}

fn profile_file_error(path: &Path, error: impl std::fmt::Display) -> HarnessError {
    HarnessError::Profile(format!("{}: {error}", path.display()))
}

/// Resolve the profile an agent starts with: `explicit`, then the registry's
/// configured default, then the built-in default.
pub(crate) fn resolve_profile(
    registry: &ProfileRegistry,
    explicit: Option<&ProfileRef>,
) -> Result<Arc<CompiledProfile>, HarnessError> {
    registry
        .resolve_for_agent(explicit)
        .map_err(|error| HarnessError::Profile(error.to_string()))
}

/// Whether profiles may run shell commands (`command` gate evaluators).
///
/// Commands run arbitrary processes, so they need the host's explicit
/// opt-in; a profile loaded from the workspace additionally needs the
/// workspace to be trusted, since anyone who can edit the repository can
/// edit it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandTrust {
    pub allowed: bool,
    pub trust_workspace: bool,
}

/// A resolved profile plus every profile it can switch to.
pub(crate) struct ProfileBundle {
    pub profile: Arc<CompiledProfile>,
    pub library: Vec<Value>,
    pub allow_commands: bool,
}

/// Resolve the profile an agent starts with and its switch closure. A switch
/// target that does not resolve, or a command-running profile the host has
/// not trusted, is an error now rather than a surprise mid-run.
pub(crate) fn resolve_bundle(
    registry: &ProfileRegistry,
    explicit: Option<&ProfileRef>,
    trust: CommandTrust,
) -> Result<ProfileBundle, HarnessError> {
    let profile = resolve_profile(registry, explicit)?;
    let closure = registry.resolve_closure(&profile).map_err(|error| {
        HarnessError::Profile(format!("profile {}: {error}", profile.reference()))
    })?;
    check_command_trust(registry, &closure, trust)?;
    Ok(ProfileBundle {
        library: closure[1..]
            .iter()
            .map(|profile| document(profile))
            .collect(),
        profile,
        allow_commands: trust.allowed,
    })
}

pub(crate) fn check_command_trust(
    registry: &ProfileRegistry,
    closure: &[Arc<CompiledProfile>],
    trust: CommandTrust,
) -> Result<(), HarnessError> {
    for profile in closure.iter().filter(|profile| profile.uses_commands()) {
        let reference = profile.reference();
        if !trust.allowed {
            return Err(HarnessError::Profile(format!(
                "profile {reference} runs command evaluators; enable them with \
                 SessionBuilder::allow_command_evaluators"
            )));
        }
        let from_workspace = registry.source_of(&reference) == Some(ProfileSource::Workspace);
        if from_workspace && !trust.trust_workspace {
            return Err(HarnessError::Profile(format!(
                "workspace profile {reference} runs command evaluators; trust the workspace \
                 with SessionBuilder::trust_workspace_commands"
            )));
        }
    }
    Ok(())
}

impl ProfileBundle {
    /// The built-in default with nothing to switch to changes nothing.
    pub fn is_default(&self) -> bool {
        !self.allow_commands
            && self.library.is_empty()
            && self.profile.content_hash == harness_core::behavior::default_profile().content_hash
    }

    pub fn command(&self) -> harness_runtime::session_runtime::SessionCommand {
        harness_runtime::session_runtime::SessionCommand::SetBehaviorBundle {
            profile: document(&self.profile),
            library: self.library.clone(),
            allow_commands: self.allow_commands,
        }
    }
}

fn document(profile: &CompiledProfile) -> Value {
    serde_json::to_value(&profile.profile).expect("profiles always serialize")
}

/// `Stop` hooks from a Claude Code / Codex hooks file, as completion-gate
/// checks.
#[derive(Debug, Clone, PartialEq)]
pub struct HooksImport {
    pub checks: Vec<harness_core::behavior::GateCheck>,
    /// Hooks that were not imported, with the reason.
    pub skipped: Vec<String>,
}

impl HooksImport {
    /// A gate running every imported check, or `None` if nothing imported.
    pub fn into_gate(
        self,
        max_continuations: u32,
    ) -> Option<harness_core::behavior::CompletionGate> {
        (!self.checks.is_empty()).then_some(harness_core::behavior::CompletionGate {
            checks: self.checks,
            max_continuations,
            on_exhausted: None,
        })
    }
}

/// Import the `Stop` hooks of a Claude Code (`.claude/settings.json`) or
/// Codex (`hooks.json`) configuration. Accepts the whole settings object or
/// just its `hooks` map.
///
/// - `command` hooks become `command` evaluators with `error_policy: pass`,
///   matching those tools, where a failing hook that is not a block does not
///   stop the agent. The session must still allow command evaluators.
/// - `prompt` hooks become `model` evaluators; `agent` hooks become `agent`
///   evaluators.
/// - Other events (tool hooks run per call, which gates do not model) and
///   other hook types are reported in `skipped`.
pub fn import_hooks(settings: &Value) -> Result<HooksImport, HarnessError> {
    use harness_core::behavior::{ErrorPolicy, EvaluatorSpec, GateCheck};

    let hooks = settings.get("hooks").unwrap_or(settings);
    let events = hooks
        .as_object()
        .ok_or_else(|| HarnessError::Profile("hooks must be an object keyed by event".into()))?;
    let mut import = HooksImport {
        checks: Vec::new(),
        skipped: Vec::new(),
    };
    for (event, groups) in events {
        let groups = groups.as_array().ok_or_else(|| {
            HarnessError::Profile(format!("hooks.{event} must be an array of matcher groups"))
        })?;
        for (group_index, group) in groups.iter().enumerate() {
            let entries = group
                .get("hooks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for (hook_index, hook) in entries.iter().enumerate() {
                let kind = hook
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                let label = format!("{event}[{group_index}].hooks[{hook_index}] ({kind})");
                if event != "Stop" {
                    import
                        .skipped
                        .push(format!("{label}: only Stop hooks map to completion checks"));
                    continue;
                }
                let text = |key: &str| {
                    hook.get(key)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|text| !text.trim().is_empty())
                };
                let evaluator = match kind {
                    "command" => text("command").map(|command| EvaluatorSpec::Command {
                        command,
                        // Hook timeouts are in seconds.
                        timeout_ms: hook
                            .get("timeout")
                            .and_then(Value::as_u64)
                            .map_or(60_000, |seconds| (seconds * 1000).clamp(1, 600_000)),
                    }),
                    "prompt" => text("prompt").map(|instructions| EvaluatorSpec::Model {
                        instructions,
                        model: text("model"),
                        transcript_messages: 10,
                    }),
                    "agent" => text("prompt").map(|instructions| EvaluatorSpec::Agent {
                        instructions,
                        tools: Vec::new(),
                        model: text("model"),
                        max_turns: 8,
                        transcript_messages: 10,
                    }),
                    _ => None,
                };
                match evaluator {
                    Some(evaluator) => import.checks.push(GateCheck {
                        id: format!("hook-stop-{}", import.checks.len() + 1),
                        require: None,
                        evaluator: Some(evaluator),
                        feedback: None,
                        error_policy: if kind == "command" {
                            ErrorPolicy::Pass
                        } else {
                            ErrorPolicy::Fail
                        },
                        metadata: serde_json::json!({ "imported_from": label }),
                    }),
                    None => import.skipped.push(format!(
                        "{label}: unsupported hook type or missing command/prompt"
                    )),
                }
            }
        }
    }
    Ok(import)
}
