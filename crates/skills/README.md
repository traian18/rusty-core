# rusty-skills

Runtime-extensible skills. A skill is a directory with a `SKILL.md` (YAML front matter naming and
describing it, then markdown instructions) plus any files it references. Dropping one into
`.harness/skills/` gives agents a new capability with no recompilation, and the `skill.load` and
`skill.read` tools let a model fetch one on demand.

## Why it exists

The compile-time plugin trait cannot be used by someone who only wants to teach the agent a
procedure. A skill is text. The model pays for its full instructions only when it needs them: the
system prompt lists names and descriptions, and the tools load the rest.

## How it works

1. `catalog` walks the skills folders and builds an in-memory `SkillCatalog`; `skill` and
   `frontmatter` parse each `SKILL.md` and its metadata.
2. `provider::SkillsContextProvider` adds each skill's name and description to the system prompt.
3. `tools::load` returns a skill's instructions; `tools::read` returns a file the skill references,
   with the allowed-tools list the skill declares.

## Key types and concepts

- **Skill**: name, description, allowed tools, instructions and files.
- **`SkillCatalog`**: the skills found, by name.
- **`SkillsContextProvider`**: puts the catalog in the prompt.

## A worked example

```text
.harness/skills/pdf-report/SKILL.md      name: pdf-report, description: ...
model calls skill.load { name: "pdf-report" }   -> the instructions
model calls skill.read { name: "pdf-report", path: "template.tex" }
```

## Public API

- `SkillCatalog`, `SkillsConfig`, `SkillsContextProvider`, `skill_tools` (the `skill.load` and `skill.read`
  executors) and `SkillError`.

## Errors

`SkillError` names a malformed or missing skill and the file it came from. One bad skill does not
hide the others.

## Design decisions

- **Filesystem discovery stays out of the agent core.**
- **Paths are confined to the skill's directory**, so `skill.read` cannot reach elsewhere.

## Testing

Parsing, discovery and tool tests use temporary skill folders. Run `cargo test -p rusty-skills`.

## In the workspace

- **Depends on:** `rusty-context`, `rusty-protocol`, `rusty-tool-api`, `rusty-workspace`
- **Used by:** `rusty-engine`

## Must never

- Execute a skill's files.
- Read outside a skill's directory.
- Put a skill's full instructions in every prompt.
