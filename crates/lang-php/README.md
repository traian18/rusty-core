# rusty-lang-php

PHP support for the repository graph: it reads PHP sources into import facts, resolves those
facts to files in the repository, and reads `composer.json` to learn the packages. It plugs into
`rusty-repo-model` through `import_support()` and `manifest_adapters()`; nothing is executed.

## Why it exists

The import graph is language-agnostic: it stores files, packages and edges. Each language
decides what an import *means* (a namespace, a directory, a module path). Keeping each language in
its own crate means adding or fixing one never touches another, and a host registers only the
languages it wants.

## How it works

1. `extract` parses a source file with tree-sitter and returns its import facts (`use` declarations and literal `require` or `include` paths).
2. `resolve` matches each fact against the files and packages the repository holds and returns a
   resolution: internal file, declared external package, possible match, unresolved, or dynamic.
3. `manifest` reads the build or package files as text and reports the packages and their declared
   dependencies.
4. `import_support()` bundles the adapter and resolver so the graph builder can register them.
5. `use` is matched by `namespace` and class declarations; only literal include paths are followed.

## Key types and concepts

- **`*ImportAdapter`**: source file to import facts.
- **`*ImportResolver`**: import fact to a resolution against the repository.
- **`manifest_adapters()`**: the package-file readers.
- **`import_support()`**: the pair a builder registers, with the file extensions the language uses.

## A worked example

```php
use App\Billing\Invoice;   // resolves via namespace and class declaration
require __DIR__ . '/helpers.php';
```

## Public API

- The `*ImportAdapter` and `*ImportResolver` types, `manifest_adapters()` and `import_support()`
  (and `source_extensions()` where one crate serves several languages).

## Errors

Nothing here returns an error to its caller: an unreadable or unparsable file yields a diagnostic
on the graph and the rest of the repository is still indexed. Unresolvable imports are resolutions
with a stated reason.

## Design decisions

- **Text, not tools.** Manifests are read as text so the result is the same on every machine and no
  build runs during indexing.
- **Conservative resolution.** A guess would corrupt blast-radius answers; an honest `possible` or
  `unresolved` does not.

## Testing

Each of `extract`, `resolve` and `manifest` has fixture tests with small sample sources. Run
`cargo test -p rusty-lang-php`.

## In the workspace

- **Depends on:** `rusty-repo-model`
- **Used by:** nothing else in the workspace

## Must never

- Run the language's build tool, package manager or compiler.
- Guess: an import it cannot place becomes an unresolved edge with a reason, not a made-up file.
- Depend on the engine, a model, or another language crate.
