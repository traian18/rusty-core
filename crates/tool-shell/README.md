# rusty-tool-shell

Cancellable command execution for agents: `shell.exec` runs a command with arguments, a working
directory and an optional timeout, and cancelling the call stops the whole process tree.

## Why it exists

Running tests, builds and tools is how an agent checks its own work, and it is also the most
dangerous thing it can do. This crate does the part that is purely mechanical and easy to get
wrong: spawning in the workspace, capping output, killing the process group on cancellation or
timeout so no orphan keeps running. Whether a command may run at all is the permission system's
decision (commands are normally `Ask`), not this crate's.

## How it works

`ExecTool::execute` parses `ExecInput { command, args, cwd, .. }`, spawns the command in its own
process group with the workspace as the default working directory, and reads stdout and stderr
concurrently into one capped buffer. It then waits for exit, for the timeout, or for
cancellation. On timeout or cancellation `kill_process_tree` signals the whole group, so
grandchildren (a test runner's workers, for instance) are stopped too. The result carries the
exit status and the output; output beyond 1 MiB is cut with a notice.

## Key types and concepts

- **`ExecTool`, `ExecInput`**: the executor and its arguments.
- **Process group**: the child is the leader of a new group (`process_group(0)`), which is what
  lets a kill reach everything it started.
- **Output cap**: one megabyte across both streams, so a chatty command cannot exhaust memory.

## A worked example

```json
{ "tool": "shell.exec", "arguments": { "command": "cargo", "args": ["test", "-p", "rusty-workspace"], "cwd": "." } }
```

If the user cancels the run while the tests execute, the `cargo` process and every test binary it
started are killed; the call returns promptly with a cancelled result.

## Public API

- `ExecTool`, `ExecInput`.

## Errors

A command that cannot be spawned (not found, not executable) is an error result naming the
reason. A non-zero exit is not an error: it is a normal result with the exit code, because the
model needs the output to decide what to do. A timeout is reported as a timeout.

## Design decisions

- **Kill the group, not the child.** A shell that spawns workers would otherwise leave them
  running after cancellation.
- **`unsafe` is limited to one `kill` call**, documented with its safety condition.
- **Permission is not this crate's job.** Keeping policy out lets hosts choose between asking,
  allowing and denying per session.

## Testing

`src/executor/tests.rs` runs real commands: output capture, exit codes, timeouts, and that a
grandchild process does not survive cancellation of its parent shell. Run
`cargo test -p rusty-tool-shell`.

## In the workspace

- **Depends on:** `rusty-tool-api`, `rusty-workspace`
- **Used by:** `rusty-engine`

## Must never

- Bypass the permission gate: a tool describes what it does, and the runtime decides whether it may.
- Reach outside what it was given (the workspace for file tools, the network for web tools).
- Depend on the engine, the runtime or another tool crate.
- Run a command the permission gate has not approved.
