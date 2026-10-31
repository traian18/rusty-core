# rusty-workspace

The `Workspace` abstraction: how tools see and change the files of a project, independent of
where those files really live. A tool depends on the trait, so the same tool works on a real
directory, on a private copy, on a git worktree, on a read-only view, or on nothing at all.

## Why it exists

Agents edit code, and some of them must not edit your code. A child agent that explores should be
unable to write; one that experiments should write to a copy; one that implements a feature in
parallel needs its own branch. If every tool talked to the disk directly, each of those cases
would need its own tools. With one trait, the host chooses the workspace when it builds a
session and the tools never know the difference.

The crate also owns the one piece of security logic every file tool shares: keeping a path the
model names inside the project root, including when the model tries a `..` trick or a symlink.

## How it works

`Workspace` has four operations on paths relative to a root: `read`, `write`, `search` and
`list_files`, plus `root()` and `mode()`. The implementations differ in where the root is:

- **`FsWorkspace`** works on a real directory. Each operation is a short sequence of steps, each
  in its own file under `src/filesystem/`:
  - `read`: resolve the path without symlinks (`path_guard`), then `read_capped`.
  - `write`: refuse if the workspace is isolated, `prepare_target` (resolve, create parent
    directories, re-check for symlinks), then `write_atomically`.
  - `search`: `search_tree` walks the tree under file-count and match-count limits.
  - `list_files`: `list_tree` walks the tree to a depth.
- **`ReadOnlyWorkspace`** wraps another workspace and refuses writes.
- **`SnapshotWorkspace`** copies a source tree into a fresh temporary directory and works there;
  the source is never touched.
- **`WorktreeWorkspace`** creates a git worktree on its own branch under
  `<repo>/.harness-worktrees/` and works there.
- **`UnboundWorkspace`** is what a session gets when the host supplied none: every operation
  fails with `WorkspaceError::Unbound`.

`nested_repositories` is separate from the trait: it finds the git repositories inside a folder,
at any depth, skipping dependency and build folders.

## Key types and concepts

- **`Workspace`**: the async trait described above.
- **`WorkspaceMode`**: `Shared` (handles on one root see each other's writes) or `Isolated`
  (writes are refused or private). `FsWorkspace::with_mode(Isolated)` refuses writes.
- **`WorkspaceError`**: `Io`, `PathTraversal`, `Isolated`, `Unbound`, `ToolFailed`, `Git`.
- **`SearchResult` / `SearchMatch`**: matches sorted by file and line, with `truncated` set when a
  limit stopped the walk, so callers never treat a partial result as complete.
- **`FileInfo`**: path, size and whether it is a directory.
- **`ToolProgress`, `ProgressPhase`, `ToolResult`**: small reporting shapes re-used by the
  runtime's traits.

## A worked example

```rust
use rusty_workspace::{FsWorkspace, ReadOnlyWorkspace, Workspace, WorkspaceError};
use std::sync::Arc;

let project = Arc::new(FsWorkspace::new("/path/to/project".into()));
project.write("notes/todo.md", "- ship it\n").await?;   // creates notes/, writes atomically

let explorer = ReadOnlyWorkspace::new(project.clone());
assert_eq!(explorer.read("notes/todo.md").await?, "- ship it\n");

let refused = explorer.write("notes/todo.md", "x").await;
assert!(matches!(refused, Err(WorkspaceError::Isolated)));

let escape = project.read("../../etc/passwd").await;
assert!(matches!(escape, Err(WorkspaceError::PathTraversal { .. })));
```

A write goes through `prepare_target` and then a temporary file in the same directory that is
renamed into place, so a reader or a crash never sees a half-written file.

## Public API

- `Workspace`, `WorkspaceMode`, `WorkspaceError`: the contract and its vocabulary.
- `FsWorkspace`, `ReadOnlyWorkspace`, `SnapshotWorkspace`, `WorktreeWorkspace`,
  `UnboundWorkspace`: the implementations.
- `SearchResult`, `SearchMatch`, `FileInfo`: results of search and listing.
- `ToolProgress`, `ProgressPhase`, `ToolResult`: reporting shapes.
- `nested_repositories` (module): `nested_repositories`, `is_repository_root`,
  `is_skipped_directory`, `NestedRepository`, and the `MAX_DEPTH`, `MAX_REPOSITORIES` limits.

## Errors

- `PathTraversal`: the path leaves the root, is absolute, or passes through a symlink. The model
  should be told the path is not allowed; it is never retried with a fix-up.
- `Isolated`: a write on a read-only or isolated workspace.
- `Unbound`: no workspace was bound to the session. It fails loudly at first use so that a
  forgotten binding is never mistaken for an empty project.
- `Io`, `Git`, `ToolFailed`: the underlying operation failed; the message says why.

## Design decisions

- **No symlinks, at all.** An existing symlink on a path is refused rather than resolved and
  compared with the root. Resolving needs the target to exist, so a dangling link would pass the
  comparison and still be followed by the write that comes next. Refusing fails closed and avoids
  a gap between checking and using the path. The same rule keeps `search` and `list_files` from
  entering symlinked directories, which would otherwise loop forever on a link to an ancestor.
- **Bounded by default.** One read is capped at 10 MiB, one search at 20,000 files and 5,000
  matches, and a capped result says so. A huge or hostile tree cannot turn one tool call into an
  unbounded traversal or allocation.
- **Atomic writes.** Temporary file plus rename in the same directory.
- **Listing paths are relative to the containing directory**, not to the root. This is existing
  behavior the tools rely on, kept as is.
- **Async git.** Worktree creation runs the blocking `git2` calls on a blocking thread so they
  never stall the async runtime.

## Testing

`src/filesystem/tests.rs` covers read and write round trips, traversal and symlink escapes
(including dangling links), the read cap, the search limits and symlink-cycle safety. The
read-only and snapshot views have small tests next to them. Run `cargo test -p rusty-workspace`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `rusty-engine`, `rusty-harness-sdk`, `rusty-runtime`, `rusty-skills`, `rusty-tool-filesystem`, `rusty-tool-shell`, `rusty-workflow-runtime`

## Must never

- Let a path reach outside the root or through a symlink.
- Know about agents, sessions or tools; it only knows files.
- Depend on any other workspace crate.
