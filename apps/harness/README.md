# harness

The standalone terminal UI for the Rusty agent harness. It runs the engine in the same process, so
neither `harnessd` nor `harnessctl` is needed. It is the quickest way to try a provider, a model, or a
change to the core with a real conversation.

## Why it exists

Every other host (IDE, daemon, SDK) needs a program around it to be exercised. This TUI is the
smallest complete host: pick a provider, account and model, chat, approve tools, and watch the activity
log. Because it links the engine directly it also shows exactly what an embedder gets.

## How it works

1. `main` parses the launch options (`--integration`, `--config-json`, `--mcp-server`,
   `--skills-dir`, and the like) and builds the engine through `harness_setup`: integrations
   registered, a JSONL session store under `.harness/sessions`, the tool set, skills and MCP servers.
2. `providers` runs the provider, account and model picker and keeps the model catalog cached.
3. `controller` owns the running session: it sends prompts, steers, cancels, resolves permissions and
   switches provider or model by starting a fresh session while keeping the earlier ones.
4. `app_state` turns engine events into what the screen shows (`events`, `runtime_events`,
   `transcript`, `activity_log`, `modal`); `render`, `markdown` and `input` draw it and read keys.

## Key types and concepts

- **Launch directory = workspace.** Start it from the repository the agent should work on.
- **Provider pill and status dot** in the header: the active provider and model, and idle, running,
  connecting or error.
- **Activity log**: a raw chronological record of every event, independent of the transcript.

## A worked example

```console
cargo run -p harness -- --integration codex
cargo run -p harness -- --mcp-server 'filesystem=npx,-y,@modelcontextprotocol/server-filesystem,/tmp'
```

The full guide (providers, commands, shortcuts, tools and permissions, storage, troubleshooting)
follows below.

## Public API

A binary. Its options are `--integration`, `--config-json`, `--mcp-server`, `--skills-dir` and the
session-store location; its commands are `/new`, `/login`, `/models`, `/context`, `/log` and `/exit`.

## Errors

Provider problems (missing key, missing CLI) are shown as a health error and never discard the active
session. Engine errors are shown in the transcript.

## Design decisions

- **In-process engine**, so there is nothing to start first.
- **Keys from the environment**, never written to session metadata.
- **Terminal restored on exit**; `reset` is the fallback after a forced kill.

## Testing

State and rendering helpers have unit tests; the engine is tested elsewhere. Run
`cargo test -p harness`.

## In the workspace

- **Depends on:** `rusty-engine`, `rusty-protocol`, `rusty-provider-anthropic`, `rusty-provider-codex`, `rusty-provider-github-copilot`, `rusty-provider-openai`, `rusty-session-store`, `rusty-workspace`
- **Used by:** nothing else in the workspace

## Must never

- Persist API keys.
- Run a tool without the permission the session policy requires.
- Reimplement engine behavior: it only drives and displays it.

## Usage guide

The `harness` app is the standalone terminal UI for testing the Rusty agent harness. It runs the engine in-process, so neither `harnessd` nor `harnessctl` is required.

### Prerequisites

- A Rust toolchain with `cargo`
- A terminal that supports an alternate screen and color
- Credentials or an authenticated CLI for at least one supported provider

Run commands from the repository root unless stated otherwise.

### Verify the workspace

Run the complete workspace test suite without filtering tests:

```console
cargo test --workspace --all-targets
```

Optionally check formatting:

```console
cargo fmt --all -- --check
```

### Start the TUI

To start with the provider picker and the default Anthropic selection:

```console
cargo run -p harness
```

To start with a specific provider selected:

```console
cargo run -p harness -- --integration codex
```

Supported integration values are:

- `anthropic`
- `openai`
- `codex`
- `github-copilot`

The directory from which the TUI is launched becomes its workspace. Start it from the repository root to let the agent work with this repository.

### Provider setup

| Provider | Local requirement | Example |
|---|---|---|
| Anthropic API | `ANTHROPIC_API_KEY` | `ANTHROPIC_API_KEY=... cargo run -p harness -- --integration anthropic` |
| OpenAI API | `OPENAI_API_KEY` | `OPENAI_API_KEY=... cargo run -p harness -- --integration openai` |
| OpenAI Codex | `codex` on `PATH`, authenticated | Run `codex login` |
| GitHub Copilot | `copilot` on `PATH`, authenticated | Run `copilot login` |

API keys are read from the environment and are not persisted in session metadata. CLI providers keep credentials in their own credential stores.

You can also provide backend configuration as JSON when launching directly:

```console
cargo run -p harness -- \
  --integration openai \
  --config-json '{"default_model":"gpt-4.1"}'
```

For ordinary use, select the provider and model in the TUI instead.

### TUI flow

On startup, select:

1. A provider
2. Its account or credential profile
3. A discovered model, or enter an exact custom model ID

An unavailable API key or missing CLI executable is shown as a provider health error. A failed connection does not discard the currently active session.

The current provider and model are always shown as a highlighted pill in the header, alongside a colored status dot (green = idle/ready, blue = running, amber = connecting, red = error).

### Switching provider or model on the fly

You are never locked into your first choice. At any point during a conversation:

- Press `Ctrl+N`, or run `/new` (or `/providers`) in the composer, to reopen the provider → account → model picker.
- Picking a new provider/model starts a fresh session with that selection while keeping every earlier session available in the sidebar (`Ctrl+Up` / `Ctrl+Down` to switch back).
- Run `/models` to force a refresh of the active provider's model catalog before picking a new model.

This works mid-conversation, not just at startup — the header pill and sidebar update immediately to reflect the active provider/model.

### Commands

Enter these commands in the prompt composer:

| Command | Action |
|---|---|
| `/new` or `/providers` | Open the provider picker to switch provider/model |
| `/login` or `/connect` | Show the authentication instruction for the selected provider |
| `/models` | Refresh the active provider's model catalog |
| `/context` | Toggle the context inspector |
| `/log` or `/logs` | Toggle the activity log — a raw, chronological record of every event the agent emitted (state transitions, backend request timing, tool-call lifecycles with arguments, permission requests, usage updates), independent of the curated transcript. Also reachable from the command palette (`Ctrl+P`) as "Activity log" |
| `/exit` or `/quit` | Exit the TUI |

### Keyboard shortcuts

| Key | Action |
|---|---|
| `Enter` | Submit a prompt or confirm a modal selection |
| `Alt+Enter` or `Ctrl+J` | Insert a newline |
| `Up` / `Down` | Navigate an open modal |
| `Esc` | Close a modal or cancel the active run |
| `Ctrl+P` | Open the command palette |
| `Ctrl+I` | Toggle the context inspector |
| `Ctrl+L` | Toggle the activity log (see `/log` above) |
| `Ctrl+N` | Switch provider/model (starts a new session), available anytime |
| `Ctrl+Up` / `Ctrl+Down` | Switch between sessions |
| `Page Up` / `Page Down` | Scroll the transcript |
| `Shift+G` | Follow the latest output |
| `Ctrl+Y` | Approve a pending tool permission |
| `Ctrl+N` | Reject a pending tool permission (only while a permission prompt is open) |
| `Ctrl+C` | Cancel the active run |
| `Ctrl+Q` | Exit the TUI |

When a permission request is visible, `Ctrl+N` rejects it; otherwise, the same shortcut opens the provider/model picker.

### Tools and permissions

The TUI exposes these workspace tools:

- `fs.read` and `workspace.search` are allowed automatically.
- `fs.edit` and `shell.exec` require explicit approval.

`shell.exec` results render `stdout`/`stderr` as real multi-line text (not one escaped-JSON line) once the command finishes; reasoning/thinking also renders in full as it streams, not just a one-line preview. Both are capped at a few thousand characters for display so one very verbose command or a long thinking block can't push everything else off-screen — the full data isn't lost, only the render is bounded. Neither streams progressively mid-command today: a shell command's output only appears once it exits, since the tool doesn't emit intermediate progress.

CLI-backed providers may execute tools through their own CLI permission and sandbox systems. Review their local configuration before testing against a sensitive workspace.

### MCP servers

Connect one or more [MCP](https://modelcontextprotocol.io) servers over stdio with repeatable `--mcp-server name=command[,arg1,arg2,...]` flags at launch:

```console
cargo run -p harness -- \
  --mcp-server 'filesystem=npx,-y,@modelcontextprotocol/server-filesystem,/tmp'
```

Every tool the server advertises is registered as `mcp.<name>.<tool>`, applies to every session for the rest of the run (including ones started later via the provider picker), and shows up in the transcript/activity log like any other tool call. This flag can't express environment variables, a working directory, or a non-default timeout — see the main [README's MCP servers section](../../README.md#mcp-servers) for the embedding API that can.

### Cleaning up old runs

`harness --cleanup` lists the sessions (idle 30 days or more) and subagent worktrees (idle 7 days or
more) under the current directory with their sizes, asks `Remove these? [y/N]`, and removes them
only on a `y`. Old task-queue checkpoints are pruned on their own when a workflow runs.

### Session storage

Sessions are stored relative to the launch directory:

```text
.harness/sessions/
```

Restarting the TUI from the same directory makes those sessions available for restore and navigation. Session metadata records provider and exact model selection, but not API keys.

### Troubleshooting

### A provider is unavailable

For API providers, confirm the variable exists in the same terminal that launches the TUI:

```console
test -n "$ANTHROPIC_API_KEY" && echo "Anthropic key is set"
test -n "$OPENAI_API_KEY" && echo "OpenAI key is set"
```

For CLI providers, confirm the executable is visible on that terminal's `PATH`:

```console
command -v claude
command -v codex
command -v copilot
```

Then authenticate with the provider CLI and restart the TUI, or use `/login` to display the expected command.

### The model list is stale or incomplete

Use `/models` to refresh it. If remote discovery fails, the TUI keeps a safe cached or fallback catalog and permits an exact custom model ID.

### The terminal is left in an unusual state

The app normally restores the terminal on exit. If the process is forcibly terminated and the shell is not restored, run:

```console
reset
```

### Tests appear as filtered out

Use `--workspace`, including the leading dashes:

```console
cargo test --workspace --all-targets
```

Running `cargo test workspace` treats `workspace` as a test-name filter.
