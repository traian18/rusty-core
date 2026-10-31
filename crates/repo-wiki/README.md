# rusty-repo-wiki

Cited documentation pages for every level of a repository (repository, folder, file, group),
rendered from published snapshots, plus the planning of which pages a model should see with a
prompt, and written guide pages built from the same facts.

## Why it exists

Documentation that is stored goes stale. Here structure is a view: a page is a pure function of
the snapshot (and the Git and findings sets that match it), so nothing needs storing or syncing, and
every fact carries the citations and links that support it. The same pages feed the IDE and the
context a model gets.

## How it works

1. `facts` reads a snapshot into the facts pages are made of.
2. `render` turns facts into `Page`s with citations, per level (`file`, `group`, folder, repository).
3. `context` plans which pages a message needs: it anchors on what the message names, what is open
   or touched, and widens along imports by task, then ranks (`ContextPlanner`).
4. `written` builds guide pages (getting started, modules, concepts, flows, diagrams), grounded in
   the facts, ready for a model or a person to polish.
5. `tests_of` maps source files to their tests.

## Key types and concepts

- **`Page` / `PageId`**: one document, identified by level and path.
- **`Citation`**: where a fact came from.
- **`Plan`**: the ranked items chosen for a message, or an abstention when nothing anchors.

## A worked example

```rust
let renderer = PageRenderer::new(graph).with_git(git).with_findings(findings);
let page = renderer.page(&PageId::file("src/session.rs"), None)?;
let plan = ContextPlanner::new(&snapshot, centrality).plan(&input);
```

## Public API

- `PageRenderer`, `Page`, `PageId`, `PageLevel`, `Section`, `Citation`, `Fact`, `TreeEntry`.
- `ContextPlanner`, `PlanInput`, `Plan`, and the `written` page builders.

## Errors

`WikiError` covers an unknown page and a snapshot that does not match. A missing Git or findings
set leaves those sections out and says so.

## Design decisions

- **No stored pages.** Rendering is cheap and always current.
- **Abstain rather than pad.** With nothing to anchor on, only ambiguous pointers are offered.

## Testing

Render tests compare pages against fixture snapshots; the planner has replay tests over recorded
prompts. Run `cargo test -p rusty-repo-wiki`.

## In the workspace

- **Depends on:** `rusty-code-analysis`, `rusty-code-health`, `rusty-findings`, `rusty-git-analysis`, `rusty-infra-analysis`, `rusty-repo-model`
- **Used by:** `rusty-context-repo`, `rusty-tool-repo`

## Must never

- Call a model while rendering a page.
- State a fact without a citation.
- Store rendered pages as the source of truth.
