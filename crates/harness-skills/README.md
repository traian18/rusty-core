# harness-skills

Runtime-extensible skills. A skill is a directory with a `SKILL.md` (YAML frontmatter naming it, then markdown instructions) plus any files it references. Dropping one into `.harness/skills/` gives agents a new capability with no recompilation, which the compile-time `Plugin` trait cannot do.

## What's inside

- `catalog` — discovery and the in-memory catalog of skills.
- `skill`, `frontmatter` — parsing a skill and its metadata.
- `provider` — `SkillsContextProvider`, which puts each skill's name and description into the system prompt so the model knows what exists.
- `error` — typed failures for malformed or missing skills.

The model pays for a skill's full text only when it needs it, through the `skill.load` and `skill.read` tools in [`harness-tool-skills`](../tools/skills). Skill discovery walks the filesystem, so this crate is deliberately kept out of `harness-core`.

## In the workspace

- **Depends on:** `harness-context`, `harness-protocol`, `harness-workspace`
- **Used by:** `harness-engine`, `harness-tool-skills`
