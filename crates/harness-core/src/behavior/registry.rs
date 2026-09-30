use std::collections::BTreeMap;
use std::sync::Arc;

use crate::orchestration::DefinitionStatus;

use super::compiler::{compile, CompiledProfile, ProfileValidationError};
use super::default::{default_profile, RESERVED_PROFILE_PREFIX};
use super::definition::{BehaviorProfile, ProfileId, ProfileRef};

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

    pub fn default_reference(&self) -> Option<&ProfileRef> {
        self.default.as_ref()
    }

    pub fn resolve(
        &self,
        reference: &ProfileRef,
    ) -> Result<Arc<CompiledProfile>, ProfileRegistryError> {
        let entry = match reference.revision {
            Some(revision) => self.entries.get(&(reference.id.clone(), revision)),
            None => self
                .entries
                .range((reference.id.clone(), 0)..=(reference.id.clone(), u64::MAX))
                .rev()
                .map(|(_, entry)| entry)
                // With drafts allowed (an editor, development), the newest
                // draft counts as the latest revision too.
                .find(|entry| match entry.compiled.profile.status {
                    DefinitionStatus::Published => true,
                    DefinitionStatus::Draft => self.allow_drafts,
                    DefinitionStatus::Deprecated => false,
                }),
        }
        .ok_or_else(|| ProfileRegistryError::NotFound(reference.clone()))?;
        if entry.compiled.profile.status == DefinitionStatus::Draft && !self.allow_drafts {
            return Err(ProfileRegistryError::DraftNotExecutable(
                entry.compiled.reference(),
            ));
        }
        Ok(entry.compiled.clone())
    }

    /// Resolution order: `explicit`, then the configured default, then the
    /// built-in `rusty.default`. A reference that does not resolve is an
    /// error, never a silent fallback.
    pub fn resolve_for_agent(
        &self,
        explicit: Option<&ProfileRef>,
    ) -> Result<Arc<CompiledProfile>, ProfileRegistryError> {
        match explicit.or(self.default.as_ref()) {
            Some(reference) => self.resolve(reference),
            None => Ok(default_profile()),
        }
    }

    /// `root` plus every profile reachable from it through `switch_profile`
    /// rules, resolved now. This is the library an agent needs to switch
    /// without a registry of its own; a missing target is an error.
    pub fn resolve_closure(
        &self,
        root: &Arc<CompiledProfile>,
    ) -> Result<Vec<Arc<CompiledProfile>>, ProfileRegistryError> {
        let mut resolved = vec![root.clone()];
        let mut seen = std::collections::BTreeSet::from([root.reference()]);
        let mut index = 0;
        while index < resolved.len() {
            for target in resolved[index].switch_targets() {
                let compiled = self.resolve(&target)?;
                if seen.insert(compiled.reference()) {
                    resolved.push(compiled);
                }
            }
            index += 1;
        }
        Ok(resolved)
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

    /// Where the registered revision `reference` resolves to came from.
    pub fn source_of(&self, reference: &ProfileRef) -> Option<ProfileSource> {
        let compiled = self.resolve(reference).ok()?;
        self.entries
            .get(&(compiled.profile.id.clone(), compiled.profile.revision))
            .map(|entry| entry.source)
    }

    /// Every non-built-in profile, for persisting a library.
    pub fn documents(&self) -> Vec<BehaviorProfile> {
        self.entries
            .values()
            .filter(|entry| entry.source != ProfileSource::BuiltIn)
            .map(|entry| entry.compiled.profile.clone())
            .collect()
    }

    /// Every registered revision, for listing in an editor.
    pub fn profiles(&self) -> impl Iterator<Item = (&Arc<CompiledProfile>, ProfileSource)> {
        self.entries
            .values()
            .map(|entry| (&entry.compiled, entry.source))
    }
}
