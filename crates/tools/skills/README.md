# harness-tool-skills

The two tools that let an agent reach into the skill catalog. The system prompt carries only each skill's name and description; these tools are how the model pays for the rest, and only for the skill it needs.

## What's inside

- `skill.load` — one skill's full instructions plus a listing of the files it bundles.
- `skill.read` — one of those bundled files, scoped to that skill's directory.

## In the workspace

- **Depends on:** `harness-skills`, `harness-tools`
- **Used by:** `harness-engine`
