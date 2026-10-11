# harness-extension-api

The one stable surface for third parties who write a tool or a model backend. Without it they would depend on `harness-tools`, `harness-runtime`, `harness-model` and `harness-generic-backend`, whose APIs shift as the runtime evolves. This crate re-exports what extension authors need and follows semver strictly.

## What's inside

- Re-exports of the tool traits and the model/backend building blocks.
- `Plugin` — a compile-time extension point.

For capabilities that need no code, use [skills](../harness-skills) instead.

## In the workspace

- **Depends on:** `harness-generic-backend`, `harness-model`, `harness-runtime`, `harness-tools`
- **Used by:** applications and external embedders (a leaf of the workspace)
