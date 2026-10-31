use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::{
    executor::{CancellationToken, ToolExecutor},
    tool_call::{ToolDescriptor, ToolError, ToolInput, ToolResult},
};

struct NamedTool(&'static str);

#[async_trait]
impl ToolExecutor for NamedTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: self.0.into(),
            name: self.0.to_owned(),
            description: String::new(),
            input_schema: json!({}),
        }
    }

    async fn execute(
        &self,
        _input: ToolInput,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::Internal)
    }
}

#[test]
fn a_registered_tool_is_found_by_its_id() {
    let registry = SimpleToolRegistry::new();
    registry
        .register_tool(Arc::new(NamedTool("fs.read")))
        .unwrap();

    assert!(registry.contains("fs.read"));
    assert!(registry.get_executor("fs.read").is_some());
    assert!(registry.get_executor("fs.write").is_none());
    assert_eq!(registry.descriptors().len(), 1);
}

#[test]
fn registering_the_same_id_twice_is_refused() {
    let registry = SimpleToolRegistry::new();
    registry
        .register_tool(Arc::new(NamedTool("fs.read")))
        .unwrap();

    let error = registry
        .register_tool(Arc::new(NamedTool("fs.read")))
        .unwrap_err();

    assert!(error.to_string().contains("fs.read"));
    assert_eq!(registry.descriptors().len(), 1, "the first tool is kept");
}

#[test]
fn a_lock_poisoned_by_a_panicking_thread_does_not_break_the_registry() {
    let registry = Arc::new(SimpleToolRegistry::new());
    let poisoner = Arc::clone(&registry);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.tools.lock().unwrap();
        panic!("poison the lock");
    })
    .join();

    registry
        .register_tool(Arc::new(NamedTool("fs.read")))
        .unwrap();
    assert!(registry.contains("fs.read"));
}
