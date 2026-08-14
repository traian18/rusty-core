//! [`SkillsContextProvider`]: puts the skill catalog into the system prompt.

use std::sync::Arc;

use async_trait::async_trait;
use harness_context::ContextProvider;
use harness_protocol::backend::ExecutionRequest;
use harness_workspace::Workspace;

use crate::catalog::SkillCatalog;

/// Appends the skill catalog — names and descriptions only — to every
/// outgoing request's system prompt.
///
/// This is the whole of the skills system's prompt footprint. Bodies stay on
/// disk until the model calls `skill.load`, so the standing cost of having
/// skills installed is one line each rather than one document each.
///
/// Being a [`ContextProvider`] rather than a change to `harness-core` is
/// deliberate and follows the same reasoning as
/// `harness_context::ContextAssemblingBackend`: `Agent::apply` is a
/// deterministic, synchronous state machine, and reading a directory of
/// markdown files is neither. Composing with a caller-supplied provider is
/// what `harness_context::ChainedContextProvider` is for.
pub struct SkillsContextProvider {
    catalog: Arc<SkillCatalog>,
}

impl SkillsContextProvider {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl ContextProvider for SkillsContextProvider {
    async fn assemble(
        &self,
        mut request: ExecutionRequest,
        _workspace: &dyn Workspace,
    ) -> ExecutionRequest {
        let catalog = self.catalog.catalog_prompt();
        if catalog.is_empty() {
            return request;
        }

        // Appended, not prepended: the session's own system prompt states
        // who the agent is, and the skill catalog is a reference list that
        // reads naturally after it. `StaticSystemPromptProvider` prepends
        // for the opposite reason — it *is* the identity.
        request.system_prompt = if request.system_prompt.trim().is_empty() {
            catalog
        } else {
            format!("{}\n\n{}", request.system_prompt, catalog)
        };
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use harness_protocol::ids::{RequestId, RunId};
    use harness_workspace::FsWorkspace;

    use crate::skill::{Skill, SkillSource};

    fn request(system_prompt: &str) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id: RunId::new(),
            system_prompt: system_prompt.to_string(),
            messages: vec![],
            tools: vec![],
            extended_thinking: false,
            params: Default::default(),
        }
    }

    fn catalog_with_one_skill() -> Arc<SkillCatalog> {
        Arc::new(SkillCatalog::from_skills([Skill {
            name: "pdf-report".to_string(),
            description: "Generate a formatted PDF report.".to_string(),
            instructions: "SECRET BODY".to_string(),
            allowed_tools: vec![],
            dir: PathBuf::from("/skills/pdf-report"),
            source: SkillSource::Explicit,
        }]))
    }

    #[tokio::test]
    async fn appends_the_catalog_and_keeps_the_original_prompt() {
        let provider = SkillsContextProvider::new(catalog_with_one_skill());
        let workspace = FsWorkspace::new(PathBuf::from("."));

        let assembled = provider
            .assemble(request("You are a helper."), &workspace)
            .await;
        assert!(assembled.system_prompt.starts_with("You are a helper."));
        assert!(assembled.system_prompt.contains("`pdf-report`"));
        assert!(assembled
            .system_prompt
            .contains("Generate a formatted PDF report."));
    }

    /// The assertion that actually pins progressive disclosure: the body is
    /// on disk, and it stays there.
    #[tokio::test]
    async fn never_leaks_instruction_bodies_into_the_prompt() {
        let provider = SkillsContextProvider::new(catalog_with_one_skill());
        let workspace = FsWorkspace::new(PathBuf::from("."));

        let assembled = provider
            .assemble(request("You are a helper."), &workspace)
            .await;
        assert!(
            !assembled.system_prompt.contains("SECRET BODY"),
            "instructions must not reach the prompt: {}",
            assembled.system_prompt
        );
    }

    #[tokio::test]
    async fn an_empty_catalog_leaves_the_request_untouched() {
        let provider = SkillsContextProvider::new(Arc::new(SkillCatalog::default()));
        let workspace = FsWorkspace::new(PathBuf::from("."));

        let assembled = provider
            .assemble(request("You are a helper."), &workspace)
            .await;
        assert_eq!(assembled.system_prompt, "You are a helper.");
    }
}
