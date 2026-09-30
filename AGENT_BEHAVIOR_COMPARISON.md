# How coding agents shape model behaviour: Claude Code, Codex CLI, OpenCode

**Purpose:** input for Rusty's behaviour/profile design (the layer inside an agent step, below `ORCHESTRATION_LAYER_DESIGN.md`).
**Researched:** 2026-09-29, from official docs and, for OpenCode and Codex, their source. Items marked *beta* or *experimental* are labelled that way by the vendor.

---

## 1. The core finding

None of the three lets you script the model's flow as a graph. Each runs one generic loop (model → tool calls → results → model …) and shapes behaviour entirely through **what goes into context and which actions are allowed**, changed in response to **events in the loop**. Claude Code's docs say it directly: CLAUDE.md is "context, not enforced configuration"; anything that must happen every time belongs in a hook.

They converge on the same layers:

| # | Layer | What it controls | Enforced or suggested |
|---|---|---|---|
| 1 | Base system prompt, chosen per model family | Identity, general working style | Suggested |
| 2 | Project instructions (CLAUDE.md / AGENTS.md hierarchy, path-scoped rules) | Conventions, project facts | Suggested |
| 3 | **Agent/profile bundles** (prompt + model + tools + permissions + limits) | "Who" the agent is right now | Tools and permissions enforced; prompt suggested |
| 4 | On-demand knowledge (skills) | Procedures loaded only when relevant | Suggested |
| 5 | **Event hooks** (react to loop events) | Inject context, deny/rewrite calls, block completion | Enforced |
| 6 | Built-in reminders (harness-injected synthetic messages) | Mode changes, step limits, state changes | Suggested, but well timed |
| 7 | Permissions / sandbox | What may execute | Enforced |
| 8 | Context hygiene (compaction prompts, subagent isolation) | What the model remembers | Enforced |

Layers 3, 5 and 6 are where "behaviour design" actually happens.

---

## 2. Claude Code

**Base prompt and styles.** A built-in software-engineering system prompt. Output styles swap in a custom prompt; the built-in coding instructions are dropped unless the style sets `keep-coding-instructions: true`. `--append-system-prompt` adds to it at launch. ([output styles](https://code.claude.com/docs/en/output-styles))

**Project instructions.** `CLAUDE.md` at managed/user/project/local scope, concatenated root-down. Subdirectory files load **when Claude reads files there**, and `.claude/rules/*.md` with `paths:` globs load only when matching files are read. This is file-triggered context injection. The content is delivered as a user message after the system prompt. ([memory](https://code.claude.com/docs/en/memory))

**Harness-added context ("system reminders").** Claude Code adds its own context mid-conversation: CLAUDE.md, output-style instructions, a note when a file Claude read changes on disk, and git attribution lines. ([how it works](https://code.claude.com/docs/en/how-claude-code-works))

**Agent bundles: subagents.** Markdown file; the body is the system prompt. Frontmatter fields: `tools`, `disallowedTools`, `model`, `permissionMode`, `maxTurns`, `skills` (preloaded), `mcpServers`, `hooks` (scoped to this agent), `memory`, `effort`, `isolation: worktree`, `omitClaudeMd`, `initialPrompt`. A subagent runs in a fresh context and returns a summary. Selection happens via its `description` (the model delegates), an @-mention, or `--agent` for the whole session. ([sub-agents](https://code.claude.com/docs/en/sub-agents))

**Skills.** `SKILL.md`. Only name + description sit in context (budget ≈1% of the window); the body loads on invocation and persists. Frontmatter fields:
- `allowed-tools` / `disallowed-tools` (only while the skill is active)
- `model`
- `paths` (auto-activate on matching files)
- `context: fork` (run in a subagent)
- `disable-model-invocation`
- `hooks`

`` !`cmd` `` injects live command output into the skill text. ([skills](https://code.claude.com/docs/en/skills))

**Hooks: the reactive layer.** ([hooks](https://code.claude.com/docs/en/hooks))

| Event | Can do |
|---|---|
| `SessionStart` | `additionalContext`, set first message |
| `UserPromptSubmit` | block the prompt, `additionalContext` |
| `PreToolUse` | `permissionDecision` allow/deny/ask/defer + reason (the reason goes back to the model), **`updatedInput`** (rewrite args), `additionalContext` |
| `PermissionRequest` | answer on the user's behalf, rewrite input |
| `PostToolUse` | **`updatedToolOutput`** (rewrite the result), `additionalContext` |
| `PostToolUseFailure` | `additionalContext` |
| `PostToolBatch` | after parallel calls, before the next model call: block the loop, `additionalContext` |
| **`Stop`** | **`decision: "block"` + `reason` → the model keeps going with the reason as feedback**; `continue: false` ends it |
| `SubagentStart` / `SubagentStop` | inject into the subagent / block its completion |
| `PreCompact` / `PostCompact`, `PreModelSwitch` / `PostModelSwitch`, `FileChanged`, `InstructionsLoaded`, … | guard or observe |

Hooks are matched by tool name (exact or regex). Hook types:
- `command`, `http`, `mcp_tool`
- *experimental*: **`prompt`** (an LLM evaluates the event) and **`agent`** (a verifier subagent)

Hooks can be set per settings layer, per subagent, or per skill.

**Gates.** Permission modes (manual, acceptEdits, auto with a classifier, plan) and allow/deny rules in settings.

---

## 3. Codex CLI

**Base prompt.** A per-model instruction template in source (`codex-rs/core/templates/model_instructions/…`) with interchangeable **personality** templates (e.g. `friendly`, `pragmatic`). Config keys:
- `model_instructions_file` replaces the built-in instructions
- `developer_instructions` adds to them

([config reference](https://learn.chatgpt.com/docs/config-file/config-reference), [source](https://github.com/openai/codex/tree/main/codex-rs/core/templates))

**Project instructions.** `AGENTS.md` / `AGENTS.override.md`: global, then git root down to the working directory, concatenated so closer files come later. Capped at 32 KiB (`project_doc_max_bytes`). Built **once at session start**; there is no path-triggered loading. ([AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md))

**Agent bundles.**
- **Profiles:** named config overlays selected with `--profile`
- **Custom agents:** TOML in `.codex/agents/` with `name`, `description`, `developer_instructions`, `model`, `model_reasoning_effort`, `sandbox_mode`, `mcp_servers`, `skills.config`
- **Subagents:** run in isolated threads and return summaries; `agents.max_concurrent_threads_per_session` caps parallelism
- An orchestrator prompt template teaches the model when to spawn, wait for and close agents

([subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents))

**Skills.** Same `SKILL.md` format (the Agent Skills standard), with per-skill enablement via `skills.config`.

**Hooks** (*beta*). Same event names and nearly the same JSON as Claude Code:
- `SessionStart`, `UserPromptSubmit`
- `PreToolUse` (block, `updatedInput`, context), `PermissionRequest`, `PostToolUse` (block with feedback, context)
- `Stop` (continue the turn with a new prompt)
- `SubagentStart` / `SubagentStop`
- `PreCompact` / `PostCompact`, `SessionEnd`, `Interrupt`

Hooks are command-only, configured in `hooks.json` or `[hooks]` in `config.toml`, and must be trusted before they run. ([hooks](https://learn.chatgpt.com/docs/hooks))

**Gates.**
- `approval_policy` (`on-request`, `never`, granular per category)
- `sandbox_mode` (`read-only`, `workspace-write`, `danger-full-access`)
- `features.*` toggles (e.g. shell tool, web search)
- per-tool enablement

**Context hygiene.** `compact_prompt` overrides the compaction prompt.

---

## 4. OpenCode

**Base prompt, chosen by model family.** `session/system.ts` picks a prompt file by model id: `anthropic.txt` for Claude, `gpt.txt`, `codex.txt`, `gemini.txt`, `kimi.txt`, `beast.txt`, … Environment details, the skill list and MCP instructions are then appended. The same agent gets a *different* base prompt per model, because models respond differently to the same instructions. ([source](https://github.com/sst/opencode/tree/dev/packages/opencode/src/session))

**Agent bundles.** JSON in `opencode.json` or markdown in `agents/`. Fields:
- `mode` (`primary` / `subagent` / `all`), `description`, `model`
- `prompt`, `temperature`, `top_p`
- **`steps`** (max loop iterations)
- `permission` (per tool, pattern-based), `hidden`

Primary agents (**Build**, **Plan**) are switched with Tab; subagents are invoked automatically or by @-mention. Hidden system agents handle compaction, titles and summaries. ([agents](https://opencode.ai/docs/agents/))

**Built-in reminders, in source** (`session/reminders.ts`). Before every model request the harness appends *synthetic* text parts to the last user message:
- while the `plan` agent is active, the plan-mode prompt
- on the first turn after switching plan → build, `build-switch.txt` ("Your operational mode has changed from plan to build… You are permitted to make file changes"), plus a pointer to the plan file if one exists
- on the last allowed step (`steps`), a `MAX_STEPS_PROMPT`

This is the cleanest example of **mode transitions implemented as context injection**.

**Plugins: the most direct control of the three.** ([plugins](https://opencode.ai/docs/plugins/), [hook types](https://github.com/sst/opencode/blob/dev/packages/plugin/src/index.ts))

| Hook | Mutates |
|---|---|
| `experimental.chat.system.transform` | **the system prompt for each request** |
| `experimental.chat.messages.transform` | **the whole message array before each request** |
| `chat.message` | incoming user message/parts |
| `chat.params` | temperature, topP, topK, max tokens per request |
| `tool.definition` | **a tool's description and parameters** |
| `tool.execute.before` / `after` | tool args / tool output |
| `permission.ask` | allow/ask/deny |
| `tool` | register new tools |
| `experimental.session.compacting` | compaction context/prompt |
| `event` | observe every bus event (`session.idle`, `file.edited`, `permission.asked`, …) |

**Gates.** Allow/ask/deny per tool with wildcard patterns (last match wins), overridable per agent. Special guards: `doom_loop` (repeated identical calls), `external_directory`, `task`, `skill`.

---

## 5. Side by side

| Capability | Claude Code | Codex | OpenCode |
|---|---|---|---|
| Base prompt per model family | one prompt + output styles | per-model templates + personalities | per-model prompt files |
| Project instructions | CLAUDE.md, **path-triggered rules** | AGENTS.md at start only | AGENTS.md |
| Agent/profile bundle | subagents (rich frontmatter) | profiles + TOML agents | primary/sub agents |
| Mode switching | permission modes, plan mode, output styles | profiles, plan mode | Tab between primary agents + switch reminders |
| Max steps per agent | `maxTurns` | — | `steps` + final-step prompt |
| Skills (on-demand) | yes (+ tool/model scope, fork, paths) | yes | yes (`skill` permission) |
| Inject context on events | hooks `additionalContext` | hooks (*beta*) | message/system transforms |
| Deny a tool call with a reason to the model | `PreToolUse` | `PreToolUse` | `tool.execute.before` / permissions |
| Rewrite tool args | `updatedInput` | `updatedInput` | `tool.execute.before` |
| Rewrite tool result | `updatedToolOutput` | — | `tool.execute.after` |
| Rewrite tool descriptions | — | — | `tool.definition` |
| Per-request sampling params | effort, per skill/agent | reasoning effort | `chat.params` |
| **Completion gate** (refuse "done", send feedback) | **`Stop` block + reason** | **`Stop` continue** | — (would need a plugin) |
| LLM-as-evaluator hook | `prompt` / `agent` hook types (*experimental*) | — | — |
| Loop detection | — | — | `doom_loop` permission |
| Declarative graph of the model flow | **no** | **no** | **no** |

---

## 6. What this means for Rusty

1. **The hook vocabulary is converging on a de facto standard.** Codex adopted Claude Code's event names and output shape (`additionalContext`, `permissionDecision`, `updatedInput`, `decision: block` + `reason`). Rusty should use the same vocabulary, both to be familiar and because it could then load existing `hooks.json` files.
2. **Everything they do is expressible as rules on loop events,** which is exactly what a canvas can edit. Their hooks are imperative scripts. The opportunity for Rusty is to make the common actions **declarative** (inject text, deny with reason, switch profile, require a check before completion) and keep `command` / LLM-evaluator actions as escape hatches.
3. **Profiles are the unit of "behaviour".** All three bundle prompt + model + tools + permissions + limits. Switching a profile mid-run is how they do "plan → build"; OpenCode shows the switch must also *announce itself* to the model with a reminder.
4. **The completion gate is the strongest behavioural lever** (Claude Code and Codex `Stop`). It maps directly onto the verify/retry idea already in Rusty's orchestration layer, but inside the loop, without restarting the attempt.
5. **Per-request context assembly is the key mechanism.** OpenCode's message/system transforms and reminders all run right before each model call. Rusty already has this seam: `ContextAssemblingBackend` / `ContextProvider` in `harness-context`, which wraps every backend request.

### Where Rusty stands today

| Needed | Rusty today |
|---|---|
| Per-request context/system transforms | `harness-context::ContextProvider` via `ContextAssemblingBackend` |
| Skills (on-demand) | `harness-skills`, `tools/skills` |
| Allow/ask/deny per tool | `ToolPolicy` / `PermissionMode`, `ExecutionPolicy` |
| Per-request params | `ExecutionParams` + `ConfigureExecution` |
| Inject input mid-run | `Steer` / `FollowUp` commands |
| Subagents | `agent.spawn`, child supervision |
| Workflow-level orchestration | `harness-core::orchestration` (**none of the three has this**) |
| **Profiles** (named bundles, switchable mid-run) | missing |
| **Event rules / hooks** (PreToolUse, PostToolUse, Stop, …) | missing |
| **Completion gate** | missing (only between steps, via Verify) |
| Tool-description overrides, rewriting args/results | missing |
| Step limit with a final-step prompt, loop detection | missing |
