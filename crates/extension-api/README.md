# rusty-extension-api

The one stable surface for third parties who write a tool or a model backend. It re-exports what
extension authors need and follows semver strictly, so the internal crates can change without breaking
them.

## Why it exists

Without it an author would depend on `rusty-tool-api`, `rusty-runtime` and `rusty-model-backend`,
whose APIs shift as the runtime evolves. One crate with a strict version policy is the contract.

## How it works

1. It re-exports the tool traits (`ToolExecutor`, `ToolDescriptor`, `ToolInput`, `ToolResult`,
   `ToolError`) and the model building blocks (`ModelClient`, `ModelRequest`, `ModelEvent`,
   `GenericModelBackend`, `ExecutionBackend`, `IntegrationFactory`).
2. `Plugin` groups what a crate contributes: tools and integration factories. An embedder collects
   plugins at startup and registers them.
3. Only compile-time registration is supported; dynamic loading is not built.

## Key types and concepts

- **`Plugin`**: `name()`, `tools()`, `integrations()`.
- **Semver gate**: a change to an internal trait that would break a plugin bumps this crate.

## A worked example

```rust
struct Weather;
impl Plugin for Weather {
    fn name(&self) -> &'static str { "weather" }
    fn tools(&self) -> Vec<Arc<dyn ToolExecutor>> { vec![Arc::new(WeatherTool)] }
}
```

## Public API

- The re-exported tool and model types, and `Plugin`.

## Errors

None of its own; tool and model errors are the re-exported types.

## Design decisions

- **Re-export, do not wrap.** Authors use the real traits.
- **Compile-time only** until there is a concrete need for dynamic loading (ABI stability,
  sandboxing and versioning are a project of their own).

## Testing

A compile-level test implements `Plugin` through the public surface only. Run
`cargo test -p rusty-extension-api`.

## In the workspace

- **Depends on:** `rusty-model-backend`, `rusty-runtime`, `rusty-tool-api`
- **Used by:** nothing else in the workspace

## Must never

- Add behavior: it is a facade.
- Expose an internal type that is not part of the contract.
