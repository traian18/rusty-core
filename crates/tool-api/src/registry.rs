//! How tool executors are found by id: the [`ToolRegistry`] contract and the in-memory
//! [`SimpleToolRegistry`] every session uses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{executor::ToolExecutor, tool_call::ToolDescriptor, tool_id::ToolId};

/// Lets the runtime look tools up without depending on one registry implementation, so tests
/// can supply their own.
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

type RegisteredTool = (ToolDescriptor, Arc<dyn ToolExecutor>);

/// A registry that keeps its tools in memory, keyed by id.
///
/// A panic in another thread while it held the lock cannot corrupt the map, so a poisoned lock
/// is still used rather than panicking again.
pub struct SimpleToolRegistry {
    tools: Mutex<HashMap<String, RegisteredTool>>,
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
    pub fn register_tool(&self, executor: Arc<dyn ToolExecutor>) -> Result<(), RegistrationError> {
        let descriptor = executor.descriptor();
        let mut tools = self.locked();
        let key = descriptor.id.as_str().to_owned();
        if tools.contains_key(&key) {
            return Err(RegistrationError::DuplicateToolId(descriptor.id));
        }
        tools.insert(key, (descriptor, executor));
        Ok(())
    }

    /// Look up a registered tool executor by ID.
    pub fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.locked()
            .get(tool_id)
            .map(|(_, executor)| executor.clone())
    }

    /// Return descriptors for all registered tools (for model-facing
    /// registration).
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.locked()
            .values()
            .map(|(descriptor, _)| descriptor.clone())
            .collect()
    }

    /// Check whether a tool ID is registered.
    pub fn contains(&self, tool_id: &str) -> bool {
        self.locked().contains_key(tool_id)
    }

    fn locked(&self) -> MutexGuard<'_, HashMap<String, RegisteredTool>> {
        self.tools.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

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

#[cfg(test)]
mod tests;
