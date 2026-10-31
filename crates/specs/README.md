# rusty-specs

Spec-driven development: the folder layout, the Markdown formats and the checks for changes that
move from a spec to a plan to finished work. Files are the source of truth; nothing here calls a model.

## Why it exists

A change that starts as a request should leave a trail a person can read and edit: what was asked,
the design, the tasks, the verification. Plain files in the repository are reviewable, diffable and
survive any tool. This crate defines their shape and reads them leniently, so a hand-edited file
produces a warning, not a failure.

## How it works

1. `layout` says where things live: the specs root, change folders, numbering, slugs, branch names.
2. `parse` reads front matter, `spec.md`, `tasks.md` and capability specs into `model` data;
   `validate` reports warnings with file and line for one change.
3. `overview` collects everything under the root with progress and next steps; `onboarding` proposes
   capabilities for an existing project; `drift` finds text naming files that no longer exist.
4. `plan` composes the task plan the workflow queue runs from the spec and the unticked tasks;
   `context` composes the text a spec workflow starts from; `linked` relates changes across a
   workspace's repositories.
5. `progress` and `record` tick tasks and move a change's status; `archive` merges a finished change
   into the capabilities and moves it to the archive.
6. `import` and `export` convert to and from OpenSpec and Spec Kit layouts.
7. `store::SpecStore` is the file access trait, with disk and in-memory implementations.

## Key types and concepts

- **Change**: a folder with `change.md`, `spec.md`, `design.md`, `tasks.md`, `checklist.md`.
- **Capability**: a long-lived spec of what the project does.
- **`SpecStore`**: the only way these functions touch files.

## A worked example

```text
specs/changes/0001-a/spec.md      ### R1: One   - C1: WHEN a THEN b
specs/changes/0001-a/tasks.md     - [ ] **T1** Model. R1 (C1)
queue_plans(&store, "0001-a")  -> a full plan and the remaining plan for the queue
```

## Public API

- `SpecStore` (`FsStore`, `MemoryStore`), `overview`, `validate`, `plan::queue_plans`, `progress`,
  `archive`, `record`, `import`, `export`, `layout` helpers.

## Errors

Parsing is lenient: problems are warnings with a location. Store failures and an unknown change are
errors.

## Design decisions

- **Files over a database**; the store trait makes tests need no disk.
- **No model calls**; workflows supply the text and `record` only files it.

## Testing

Format, plan, archive and import/export tests run on the in-memory store. Run
`cargo test -p rusty-specs`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** nothing else in the workspace

## Must never

- Call a model or the network.
- Rewrite a file the user edited without a recorded step.
- Depend on the engine.
