//! A step's scoped tools plus `request_research`.

use std::sync::Arc;

use rusty_runtime::traits::ToolRegistry;

use super::RESEARCH_TOOL;

pub(in crate::session_agent_executor) struct WithResearch {
    pub(in crate::session_agent_executor) inner: Arc<dyn ToolRegistry>,
    pub(in crate::session_agent_executor) research: Arc<dyn rusty_tool_api::ToolExecutor>,
}

impl ToolRegistry for WithResearch {
    fn register(
        &self,
        executor: Arc<dyn rusty_tool_api::ToolExecutor>,
    ) -> Result<(), rusty_tool_api::RegistrationError> {
        Err(rusty_tool_api::RegistrationError::DuplicateToolId(
            executor.descriptor().id,
        ))
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn rusty_tool_api::ToolExecutor>> {
        if tool_id == RESEARCH_TOOL {
            return Some(self.research.clone());
        }
        self.inner.get_executor(tool_id)
    }

    fn descriptors(&self) -> Vec<rusty_tool_api::ToolDescriptor> {
        let mut descriptors = self.inner.descriptors();
        descriptors.push(self.research.descriptor());
        descriptors
    }
}
