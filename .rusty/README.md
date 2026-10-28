<!-- Written by Rusty; remove this line to keep your own version. -->
# .rusty

What Rusty keeps for this project. The shared parts are meant to be committed with it; the
others are this machine's own and `.gitignore` here keeps them out of Git. If the
repository's own `.gitignore` ignores `/.rusty`, remove that line to share the shared parts.

| Path | Shared | What goes into it |
|---|---|---|
| `README.md` | yes | this guide to the folder |
| `.gitignore` | yes | what Git keeps of this folder: the shared parts only |
| `guidelines/` | yes | the project's code guidelines: rules.json (patterns, size and complexity limits, import boundaries; checked on every edit and before a run finishes) and style.md (the prose style guide the agent is shown before it edits a file) |
| `specs/` | yes | spec-driven changes: each change's proposal, design and task list, and the capability specs (Specs page) |
| `specs.json` | yes | where the specs live |
| `skills/` | yes | the project's own skills |
| `workflows/` | yes | saved workflows (Behaviors page) |
| `profiles/` | yes | behavior profiles, and config.json naming the default one |
| `canvas/` | yes | canvas pipelines and their layout |
| `chats/` | no | the Agent tab's conversations, one file per chat |
| `runs/` | no | event logs of agent and workflow runs |
| `goals/` | no | goal loops (/goal): each goal's targets, plan, measurements and journal; <id>.checkpoint.json names the snapshot the running pass started from |
| `findings/` | no | long answers the agent saved as documents |
| `metrics/` | no | usage and code health history |
| `observability/` | no | traces of model and tool calls |
| `notepad.txt` | no | the notepad |
