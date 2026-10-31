# rusty-tool-filesystem

The file tools an agent uses to work on a project: `fs.read`, `fs.edit` and `workspace.search`.
They are written against the `Workspace` trait, so the same tools work on a real directory, an
IDE's virtual file system, a private copy or a git worktree.

## Why it exists

Reading and editing files is the most common thing an agent does, and the one that most needs
to be safe: a path must never escape the project and an edit must never half-apply. Putting the
tools on `Workspace` means the safety rules live once, in `rusty-workspace`, and a child agent
that is given a read-only workspace simply cannot write, with no special-casing in the tools.

## How it works

Each tool is a `ToolExecutor` (`rusty-tool-api`) that parses its arguments, calls one `Workspace`
operation, and returns a result the model can read.

- **`fs.read`** reads one file relative to the workspace root (capped, with a notice when
  truncated).
- **`fs.edit`** either writes whole `content`, or replaces `old_text` with `new_text`. The match
  is strict: `old_text` must appear exactly once, with no fuzzy matching and no "replace all", so
  an edit can never silently hit the wrong place. A missing or ambiguous `old_text` is reported so
  the model can correct it.
- **`workspace.search`** finds a case-insensitive substring in text files and returns matches
  with file, line and content, flagged `truncated` when a limit stopped the search.

## Key types and concepts

- **`ReadTool`, `EditTool`, `SearchTool`**: the three executors, each built over an
  `Arc<dyn Workspace>`.
- **`ReadInput`, `EditInput`, `SearchInput`**: the argument structs (`path`; `path` with either
  `content` or `old_text` and `new_text`; `query`).
- **Failure convention**: a bad path, a missing file or an ambiguous edit is a `ToolResult` with
  `is_error: true`, which the model can read and retry; a `ToolError` is reserved for faults.

## A worked example

```json
{ "tool": "fs.read", "arguments": { "path": "src/main.rs" } }
{ "tool": "fs.edit", "arguments": { "path": "src/main.rs", "old_text": "println!(\"hi\")", "new_text": "println!(\"hello\")" } }
{ "tool": "workspace.search", "arguments": { "query": "TODO" } }
```

A read of `../../etc/passwd` returns an error result saying the path is outside the workspace; it
never reaches the disk.

## Public API

- `ReadTool`, `EditTool`, `SearchTool` and their input types.

## Errors

Workspace errors map to error results: `PathTraversal` becomes "path outside the workspace",
`Isolated` becomes "this workspace is read-only", `Unbound` tells the model no workspace is bound.
Only an infrastructure fault is a `ToolError`.

## Design decisions

- **The tools hold no path logic.** All containment and symlink rules are `rusty-workspace`'s.
- **Errors are readable text.** The model fixes its own mistakes instead of the run failing.

## Testing

Unit tests beside each tool use a temporary directory as the workspace and cover success, missing
files, traversal attempts and ambiguous edits. Run `cargo test -p rusty-tool-filesystem`.

## In the workspace

- **Depends on:** `rusty-tool-api`, `rusty-workspace`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
