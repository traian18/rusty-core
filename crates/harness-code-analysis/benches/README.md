# Call-graph benchmark

Run from `rusty-core`:

```text
cargo bench -p harness-code-analysis --bench call_graph
```

The executable generates a temporary synthetic workspace of 1,000 small source files (200 each of
Python, JavaScript, Java, Go and Rust). Each module defines a helper and a function that calls the
helper and the previous module's function, so the workspace contains same-file and cross-file
edges. It reports:

- workspace composition, file count and source bytes;
- callable, call, resolved and unresolved counts;
- cold refresh, unchanged (fully cached) refresh, and a refresh after editing one file, with cache
  hits/misses;
- a query limited to 50 callables;
- peak resident set size before and after (Unix only; `None` elsewhere). It is a process-wide
  high-water mark, so it bounds rather than isolates the analysis.

The output is a single-run observation, not a statistical distribution or an SLA. Timings depend on
the machine, filesystem and build profile. The workspace is synthetic and far more regular than a
production repository; agree numerical budgets only after measuring representative workspaces.
