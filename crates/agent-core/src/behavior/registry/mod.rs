use std::collections::BTreeMap;
use std::sync::Arc;

use crate::behavior::DefinitionStatus;

use crate::behavior::compiler::{compile, CompiledProfile, ProfileValidationError};
use crate::behavior::default::{default_profile, RESERVED_PROFILE_PREFIX};
use crate::behavior::definition::{BehaviorProfile, ProfileId, ProfileRef};

/// Where a profile came from. Later variants take precedence when the same
/// id and revision are registered from several sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProfileSource {
    BuiltIn,
    Workspace,
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileRegistryError {
    #[error("{0}")]
    Invalid(ProfileValidationError),
    #[error("profile {0} is published and cannot be changed; register a new revision")]
    ImmutableRevision(ProfileRef),
    #[error("profile id {0} is reserved for built-in profiles")]
    Reserved(ProfileId),
    #[error("profile {0} is not registered")]
    NotFound(ProfileRef),
    #[error("profile {0} is a draft and drafts are not executable")]
    DraftNotExecutable(ProfileRef),
}

#[derive(Debug, Clone)]
struct Entry {
    compiled: Arc<CompiledProfile>,
    source: ProfileSource,
}

/// Compiled, versioned behavior profiles.
///
/// - Built-ins (ids starting with `rusty.`) are always present and cannot be
///   shadowed.
/// - For the same id and revision, `Host` beats `Workspace` beats `BuiltIn`.
/// - Within one source a published revision is immutable; a draft may be
///   replaced, which is what an editor does while a profile is being written.
#[derive(Debug, Clone)]
pub struct ProfileRegistry {
    entries: BTreeMap<(ProfileId, u64), Entry>,
    default: Option<ProfileRef>,
    allow_drafts: bool,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileRegistry {
    /// A registry holding the built-in profiles.
    pub fn new() -> Self {
        let builtin = default_profile();
        let key = (builtin.profile.id.clone(), builtin.profile.revision);
        Self {
            entries: BTreeMap::from([(
                key,
                Entry {
                    compiled: builtin,
                    source: ProfileSource::BuiltIn,
                },
            )]),
            default: None,
            allow_drafts: false,
        }
    }

    /// Permit resolving draft revisions (development / editor preview).
    pub fn allow_drafts(mut self, allow: bool) -> Self {
        self.allow_drafts = allow;
        self
    }

    pub fn register(
        &mut self,
        profile: BehaviorProfile,
        source: ProfileSource,
    ) -> Result<Arc<CompiledProfile>, ProfileRegistryError> {
        if source != ProfileSource::BuiltIn
            && profile.id.as_str().starts_with(RESERVED_PROFILE_PREFIX)
        {
            return Err(ProfileRegistryError::Reserved(profile.id));
        }
        let compiled = Arc::new(compile(profile).map_err(ProfileRegistryError::Invalid)?);
        let key = (compiled.profile.id.clone(), compiled.profile.revision);
        if let Some(existing) = self.entries.get(&key) {
            if existing.source > source {
                return Ok(existing.compiled.clone());
            }
            if existing.source == source
                && existing.compiled.profile.status != DefinitionStatus::Draft
                && existing.compiled.content_hash != compiled.content_hash
            {
                return Err(ProfileRegistryError::ImmutableRevision(
                    existing.compiled.reference(),
                ));
            }
        }
        self.entries.insert(
            key,
            Entry {
                compiled: compiled.clone(),
                source,
            },
        );
        Ok(compiled)
    }

    /// The profile used when an agent names none (e.g. from
    /// `.rusty/profiles/config.json`). Validated when resolved.
    pub fn set_default(&mut self, reference: Option<ProfileRef>) {
        self.default = reference;
    }

    /// A registry holding exactly `profiles` (plus the built-ins), for an
    /// agent's switch library. Drafts are allowed: the host already chose them.
    pub fn from_library(
        profiles: impl IntoIterator<Item = BehaviorProfile>,
    ) -> Result<Self, ProfileRegistryError> {
        let mut registry = Self::new().allow_drafts(true);
        for profile in profiles {
            if profile.id.as_str().starts_with(RESERVED_PROFILE_PREFIX) {
                continue;
            }
            registry.register(profile, ProfileSource::Host)?;
        }
        Ok(registry)
    }
}

mod lookup;
