# harness-tool-filesystem

The file tools an agent uses to work on a project, implemented against the `Workspace` trait so they work on real disk or an IDE's virtual file system alike.

## What's inside

- `fs.read` — read a file.
- `fs.edit` — apply a precise edit.
- `workspace.search` — search file contents.

## In the workspace

- **Depends on:** `harness-tools`, `harness-workspace`
- **Used by:** `harness-engine`
