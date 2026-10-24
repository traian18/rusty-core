# Specs

This folder holds the specs of the project. A change moves from a spec to a plan to finished work,
and every step leaves Markdown here, so a person or any coding agent can pick it up.

```text
specs/
  project.md                 purpose, stack, commands, conventions and principles
  capabilities/<name>/spec.md  what the system does now, as named requirements
  changes/NNNN-slug/         one folder per change
    change.md                status and history
    spec.md                  why, what, requirements with criteria, open questions
    research.md              optional: sourced options
    design.md                approach, files, data, risks
    tasks.md                 the ordered checklist
    tasks/T1-slug.md         one document per task: plan, what was built, review
    verification.md          every criterion with its result
  archive/                   finished changes, merged into capabilities
```

## Working in a change

1. Read `specs/project.md`, its principles first.
2. Read the change's `spec.md`; answer or ask about anything under *Open questions*.
3. Build from `tasks.md` in order, one task at a time, and tick a task when it is done and checked.
4. A task is done when the criteria it names hold. Run the project's checks before ticking.

## Formats

`spec.md` requirements are `### R1: Title`, followed by criteria such as
`- C1: WHEN the user does X THEN Y happens.` An optional `Capability: name` line says which
capability the requirement adds to or changes.

`tasks.md` has one line per task: `- [ ] **T1** Title. R1 (C1, C2) after T0`, with the instructions
indented below it.
