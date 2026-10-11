# harness-tools

The canonical tool contract. A tool is a `ToolExecutor`; everything an agent can call (built-in, MCP, skills, or IDE-provided) goes through this one seam, so adding a tool never needs a change in the runtime.

## What's inside

- `executor` — `ToolExecutor`, `ToolDescriptor`, `ToolInput`, `ToolResult`, progress reporting and failure kinds.
- `registry` — the `ToolRegistry` trait and `SimpleToolRegistry`.
- `call_context` — task-local access to the current tool call and session IDs from inside an executor.

Concrete tools live in their own crates under `crates/tools/`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `harness-engine`, `harness-extension-api`, `harness-runtime`, `harness-tool-filesystem`, `harness-tool-git`, `harness-tool-mcp`, `harness-tool-shell`, `harness-tool-skills`, `harness-tool-web`
