# harness-tool-git

Read-only git introspection backed directly by `git2`. Mutating git history (commit, add, push, branch) is deliberately out of scope: it belongs behind an explicit, visible permission check, and `shell.exec` already covers it.

## What's inside

- `git.status`, `git.diff`, `git.log`, `git.show`.

## In the workspace

- **Depends on:** `harness-tools`
- **Used by:** `harness-engine`
