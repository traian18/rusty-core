# rusty-tool-git

Read-only git introspection backed directly by `git2`: `git.status`, `git.diff`, `git.log` and
`git.show`. Mutating history (commit, add, push, branch) is deliberately out of scope: it belongs
behind an explicit, visible permission check, and `shell.exec` already covers it.

## Why it exists

An agent that is about to change code needs to see what already changed. Running `git` through the
shell works but prints unbounded text; these tools return structured, size-limited answers an
agent can act on, and they cannot modify the repository.

## How it works

Each tool opens the repository with `git2` and returns JSON. Diffs go through `patch`:

1. `DiffFilters` carries the request: pathspecs, `hunk_contains`, per-file hunk limit, file limit,
   byte limit, and `summary_only`.
2. `patch::render` walks the diff file by file under those budgets (`render.rs`), asking
   `file_block` for each file's patch text and `summary` for its summary entry.
3. What does not fit is left out and *reported* under `limits`, with a hint on how to narrow the
   request. Patch lines are never rewritten: they are included exactly as git produced them or
   omitted.

## Key types and concepts

- **`DiffFilters`**: the shared request for `git.diff` and `git.show`.
- **File summary**: path, status, additions, deletions, hunks, and whether the patch is `full`,
  `partial`, `omitted`, `filtered_out` or `summary`.
- **Limits**: hard caps (50 000 bytes of patch, 200 files with patches, 500 listed summaries)
  that apply whatever is requested.

## A worked example

```json
{ "tool": "git.diff", "arguments": { "paths": ["src/"], "hunk_contains": "unwrap", "max_hunks_per_file": 3 } }
```

The result lists each changed file with counts, includes only hunks with an added or removed
line containing `unwrap`, and, if the byte budget ran out, ends the patch with
`... (diff truncated)` and a `limits` entry saying how many files were omitted.

## Public API

- `GitStatusTool`, `GitDiffTool`, `GitLogTool`, `GitShowTool`, `DiffFilters`.

## Errors

An unreadable repository, an unknown revision or a failed diff is an error result with the git
message. Limits are not errors: they are reported inside a successful result.

## Design decisions

- **Read-only by construction.** No code path writes to the repository.
- **Omissions are always reported.** The model is told what it cannot see and how to ask for it.
- **Text is never rewritten.** Truncation cuts on a character boundary and adds a marker.

## Testing

`src/patch/tests.rs` covers limit clamping and character-boundary truncation; tests in
`diff.rs`, `show.rs`, `log.rs` and `status.rs` run against temporary repositories. Run
`cargo test -p rusty-tool-git`.

## In the workspace

- **Depends on:** `rusty-tool-api`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
- Write to the repository.
