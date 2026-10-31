# rusty-extension-api

The one stable surface for third parties who write a tool or a model backend. Without it they would depend on `rusty-tool-api`, `rusty-runtime`, `harness-model` and `rusty-model-backend`, whose APIs shift as the runtime evolves. This crate re-exports what extension authors need and follows semver strictly.

## What's inside

- Re-exports of the tool traits and the model/backend building blocks.
- `Plugin` — a compile-time extension point.

For capabilities that need no code, use [skills](../rusty-skills) instead.

## In the workspace

- **Depends on:** `rusty-model-backend`, `harness-model`, `rusty-runtime`, `rusty-tool-api`
- **Used by:** applications and external embedders (a leaf of the workspace)
