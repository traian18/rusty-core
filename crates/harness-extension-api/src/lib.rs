#![warn(clippy::all)]

//! Stable extension and plugin-facing API.
//!
//! Writing a new tool or model backend today means depending directly on
//! `harness-tools`, `harness-runtime`, and `harness-model` — three internal
//! crates whose APIs shift as the runtime evolves. This crate is the one
//! surface third-party tool/backend authors should depend on instead.
//!
//! # Semver policy
//!
//! This crate follows semver strictly and is the *only* crate third-party
//! authors should depend on for writing tools or backends — `harness-tools`,
//! `harness-runtime`, `harness-model`, and `harness-generic-backend` remain
//! implementation details that can change between workspace versions without
//! a corresponding bump here. When an internal trait changes in a way that
//! would break a [`Plugin`] implementation, this crate's version bumps
//! accordingly even if the internal crates don't follow the same discipline.
//!
//! # Scope
//!
//! This first version covers **compile-time** registration only: a
//! third-party crate implements [`ToolExecutor`] and/or [`IntegrationFactory`]
//! plus [`Plugin`], and an embedder links it in and collects it into
//! `Vec<Box<dyn Plugin>>` at startup. Dynamic loading (dylib/WASM) is a much
//! bigger undertaking — ABI stability across a dylib boundary, sandboxing,
//! versioning — and isn't built here; it's a natural extension once there's
//! a concrete need for loading plugins without recompiling the host.

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tool authoring surface (stable subset of harness-tools)
// ---------------------------------------------------------------------------

pub use harness_tools::{
    CancellationToken, ExecutionFailure, FailureKind, ProgressPhase, ToolDescriptor, ToolError,
    ToolExecutor, ToolId, ToolInput, ToolProgress, ToolResult, ToolUsage,
};

// ---------------------------------------------------------------------------
// Backend authoring surface (stable subset of harness-runtime + harness-model)
// ---------------------------------------------------------------------------

pub use harness_generic_backend::GenericModelBackend;
pub use harness_model::{ModelCapabilities, ModelClient, ModelError, ModelEvent, ModelRequest, ModelResult};
pub use harness_runtime::traits::ExecutionBackend;
pub use harness_runtime::IntegrationFactory;

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Compile-time plugin registration surface.
///
/// A plugin contributes tools, integrations, or both — every method has an
/// empty default so a plugin that only adds one kind of extension doesn't
/// need to implement the other. An embedder collects `Vec<Box<dyn Plugin>>`
/// at startup and folds `.tools()` into its tool registry and
/// `.integrations()` into its [`IntegrationFactory`] registry.
pub trait Plugin: Send + Sync {
    /// A short, human-readable name for diagnostics/logging.
    fn name(&self) -> &'static str;

    /// Tool executors this plugin contributes.
    fn tools(&self) -> Vec<Arc<dyn ToolExecutor>> {
        Vec::new()
    }

    /// Integration factories this plugin contributes.
    fn integrations(&self) -> Vec<Arc<dyn IntegrationFactory>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use harness_tools::{ToolDescriptor as Descriptor, ToolInput as Input, ToolResult as Result_};
    use serde_json::json;

    /// A trivial tool proving the re-exported surface is sufficient to
    /// implement `ToolExecutor` without reaching back into `harness-tools`
    /// directly.
    struct EchoTool;

    #[async_trait]
    impl ToolExecutor for EchoTool {
        fn descriptor(&self) -> Descriptor {
            Descriptor {
                id: ToolId::new("example.echo"),
                name: "Echo".to_string(),
                description: "Echoes its input back".to_string(),
                input_schema: json!({ "type": "object" }),
            }
        }

        async fn execute(&self, input: Input, _cancel: CancellationToken) -> std::result::Result<Result_, ToolError> {
            Ok(Result_ {
                call_id: "example.echo".to_string(),
                output: input.arguments,
                is_error: false,
            })
        }
    }

    struct EchoPlugin;

    impl Plugin for EchoPlugin {
        fn name(&self) -> &'static str {
            "echo-plugin"
        }

        fn tools(&self) -> Vec<Arc<dyn ToolExecutor>> {
            vec![Arc::new(EchoTool)]
        }
    }

    #[tokio::test]
    async fn plugin_contributes_a_working_tool_via_the_re_exported_surface() {
        let plugin = EchoPlugin;
        assert_eq!(plugin.name(), "echo-plugin");
        let tools = plugin.tools();
        assert_eq!(tools.len(), 1);

        let result = tools[0]
            .execute(
                Input {
                    arguments: json!({ "hello": "world" }),
                },
                CancellationToken::new(),
            )
            .await
            .expect("execute should succeed");
        assert_eq!(result.output, json!({ "hello": "world" }));
    }

    #[test]
    fn plugin_default_integrations_is_empty() {
        let plugin = EchoPlugin;
        assert!(plugin.integrations().is_empty());
    }
}
