//! Finding profiles in a registry.

use std::sync::Arc;

use super::{ProfileRegistry, ProfileRegistryError, ProfileSource};
use crate::behavior::{
    compiler::CompiledProfile,
    default::default_profile,
    definition::{BehaviorProfile, ProfileRef},
    DefinitionStatus,
};

impl ProfileRegistry {
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
