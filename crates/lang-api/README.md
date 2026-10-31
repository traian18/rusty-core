# rusty-lang-api

Rust and TypeScript/JavaScript import adapters and resolvers for the repository graph. Unlike the
other language crates it covers two ecosystems, because both are used by the project itself and
share module-tree style resolution.

## Why it exists

The graph needs to know who imports whom in the languages this workspace is written in. Rust needs
a module tree (`mod`, `use`, workspace crates); TypeScript needs `tsconfig` paths, `package.json`
workspaces and extension probing. Both are resolved without running Cargo or Node.

## How it works

1. `rust_extract` reads `use`, `mod` and `extern crate` declarations; `rust_resolve` builds the
   module tree of each crate (`module_tree`, `use_paths`) and resolves paths through it, including
   workspace member crates.
2. `ts_extract` reads `import`, `export … from`, dynamic `import()` and `require`; `ts_config` reads
   `tsconfig.json` (`paths`, `baseUrl`, `extends`); `ts_resolve` applies them and probes extensions
   and index files.
3. The adapters and resolvers are registered with the graph builder like any other language.

## Key types and concepts

- **`RustImportAdapter` / `RustImportResolver`** and **`TypeScriptImportAdapter` /
  `TypeScriptImportResolver`**.
- **Module tree**: the crate's modules as declared, used to resolve `crate::`, `super::` and `self::`.

## A worked example

```text
use crate::session::Store;      -> src/session/mod.rs or src/session.rs
import { x } from "@app/util"   -> the file tsconfig paths map "@app/*" to
```

## Public API

- `RustImportAdapter`, `RustImportResolver`, `TypeScriptImportAdapter`, `TypeScriptImportResolver`.

## Errors

Unparsable files and unreadable configs become graph diagnostics. An import that cannot be placed
is an unresolved or external resolution, never an error.

## Design decisions

- **Config as text.** `tsconfig.json` and Cargo manifests are parsed, not executed.
- **One crate for two ecosystems** only while they stay small; each has its own extract/resolve pair.

## Testing

Fixture tests per extractor and resolver, including path aliases and nested modules. Run
`cargo test -p rusty-lang-api`.

## In the workspace

- **Depends on:** `rusty-repo-model`
- **Used by:** nothing else in the workspace

## Must never

- Run the language's build tool, package manager or compiler.
- Guess: an import it cannot place becomes an unresolved edge with a reason, not a made-up file.
- Depend on the engine, a model, or another language crate.
