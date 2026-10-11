# harness-workspace

The `Workspace` abstraction: how tools see files and run in a project, independent of where the files really live. Tools depend on the trait, so the same tool works against a real directory, an IDE's virtual file system, a snapshot, or a git worktree.

## What's inside

- `Workspace` trait — `read`, `write`, `search` and `list_files` relative to a root, plus its root and mode (`Shared` or `Isolated`).
- `FsWorkspace` — the real-disk implementation.
- `SnapshotWorkspace`, `WorktreeWorkspace`, read-only `IsolatedWorkspace` — isolated variants for child agents (a temp copy, a git worktree, or reads only).
- `UnboundWorkspace` — the default when a host provides none: tools that never touch it behave as before, and tools that do get an actionable error instead of silent misses.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `harness-engine`, `harness-runtime`, `harness-skills`, `harness-tool-filesystem`, `harness-tool-shell`, `rusty-harness-sdk`
