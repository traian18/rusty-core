//! Tool registry types — [`ToolRegistry`] trait and [`SimpleToolRegistry`] impl.
//!
//! The [`ToolRegistry`] trait defines how tool executors are looked up by
//! their identifier string.  [`SimpleToolRegistry`] is the canonical
//! HashMap-backed implementation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::executor::{ToolDescriptor, ToolExecutor, ToolId};

/// Describes how a registry must behave — enables mocking in tests.
#[async_trait]
pub trait ToolRegistry: Send + Sync {
    /// Register a tool executor.
    ///
    /// Returns `Err` if a tool with the same `id` is already registered.
    fn register(&self, executor: Arc<dyn ToolExecutor>) -> Result<(), RegistrationError>;

    /// Look up a registered tool executor by its identifier string
    /// (e.g. `"fs.read"`).
    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>>;

    /// Return descriptors for all registered tools.
    fn descriptors(&self) -> Vec<ToolDescriptor>;
}

/// HashMap-backed concrete implementation.
pub struct SimpleToolRegistry {
    tools: Mutex<HashMap<String, (ToolDescriptor, Arc<dyn ToolExecutor>)>>,
}

impl SimpleToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool executor and its descriptor.
    ///
    /// Returns `Err` if a tool with the same `id` is already registered.
    pub fn register_tool(
        &self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), RegistrationError> {
        let descriptor = executor.descriptor();
        let mut tools = self.tools.lock().unwrap();
        let tool_id_str = descriptor.id.as_str().to_string();
        if tools.contains_key(&tool_id_str) {
            return Err(RegistrationError::DuplicateToolId(descriptor.id));
        }
        tools.insert(tool_id_str, (descriptor, executor));
        Ok(())
    }

    /// Look up a registered tool executor by ID.
    pub fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        let tools = self.tools.lock().unwrap();
        tools.get(tool_id).map(|(_, executor)| executor.clone())
    }

    /// Return descriptors for all registered tools (for model-facing
    /// registration).
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        let tools = self.tools.lock().unwrap();
        tools
            .values()
            .map(|(descriptor, _)| descriptor.clone())
            .collect()
    }

    /// Check whether a tool ID is registered.
    pub fn contains(&self, tool_id: &str) -> bool {
        let tools = self.tools.lock().unwrap();
        tools.contains_key(tool_id)
    }
}

#[async_trait]
impl ToolRegistry for SimpleToolRegistry {
    fn register(&self, executor: Arc<dyn ToolExecutor>) -> Result<(), RegistrationError> {
        self.register_tool(executor)
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.get_executor(tool_id)
    }

    fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.descriptors()
    }
}

/// Errors that can occur when registering a tool.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    /// A tool with the same ID is already registered.
    #[error("tool with id '{0}' is already registered")]
    DuplicateToolId(ToolId),
}

impl Default for SimpleToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
