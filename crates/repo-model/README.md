# rusty-repo-model

What the system knows about a repository, as data and the rules that derive it: its packages and
manifests, which file imports which, which framework wiring links files (routes, injected
providers, rendered components), how the code groups into dependency groups, and the folder tree.
Everything is built into one immutable **snapshot** that tools, the wiki and the IDE query.

It reads the repository (bounded, never following symlinks) but does not know any programming
language: per-language knowledge plugs in through traits, implemented by the `rusty-lang-*` and
`rusty-frameworks` crates.

## Why it exists

Agents and people ask the same questions of a repository: what are its packages, what depends on
this file, what breaks if I change it, where does this route get handled. Answering them needs
one consistent picture, built once per change and shared, not re-derived by every tool.
Separating the *model* of that picture from the *languages* that fill it keeps the model small and
stable while support for languages grows by adding crates. It also keeps this crate independent of
the agent engine: nothing here runs an agent, and `check-layers` enforces that.

## How it works

`RepositoryGraphBuilder` composes a `RepositoryGraphService` from replaceable parts, and
`refresh` builds a snapshot in four stages, each cached against the stage before it:

1. **Discovery** (`repository_intelligence::discovery`). A bounded walk finds manifests, source
   files and compiler configs using contained reads: each path component is opened relative to the
   previous directory with `O_NOFOLLOW`, so a symlink swapped in mid-scan is refused. Manifest
   adapters turn `Cargo.toml` and `package.json` into packages and workspace membership.
2. **Imports** (`import_graph`). For every source file a language adapter extracts import facts
   (cached by file content), compiler configs are loaded, and each language's resolver turns
   facts into edges between files. The result is an `ImportIndex`.
3. **Groups** (`dependency_groups`). Rules derive groups from the structure and the import index:
   declared packages, source roots, directories, and import cycles (iterative Tarjan, safe for very
   deep chains), plus edges that cross group boundaries.
4. **Frameworks** (`framework_relations`). Each framework adapter that can show it applies to a
   package emits relations with a rule, a version and source evidence. Conventions that cannot be
   read statically become diagnostics, never guesses.

The snapshot gets a deterministic id from the structure and the three indexes
(`combine_with_imports`, `combine_with_groups`, `combine_with_frameworks`), so identical
repositories give identical ids. Queries are paged and snapshot-bound: a cursor from one snapshot
is refused on another.

## Key types and concepts

- **`RepositoryGraph`**: the read side consumers hold as `Arc<dyn RepositoryGraph>`: `current`,
  `get`, structure and capability queries, `dependencies` / `dependents`, `groups`,
  `group_members`, `boundary_edges`, `framework_relations`, and a `graph` projection.
- **`RepositorySnapshot`**: the structure (`graph`) plus imports, groups and framework relations,
  with an id.
- **`RootRelativePath`**: a validated, `/`-separated path relative to the repository root; the
  only way paths cross this crate's API.
- **`ImportIndex`, `ImportEdge`, `ImportFact`**: the file-level import graph and how it was
  resolved; `LanguageImportAdapter` and `ImportResolver` are the traits a language implements.
- **`Group`, `GroupKind`, `GroupBasis`**: a set of files with a reason it is a set (`Declared`
  by a manifest, or `Inferred` from folders).
- **`FrameworkRelation`**: `from` file, `to` file or symbol, kind (route, provider, component,
  command), certainty and evidence.
- **`DiscoveryLimits`, `ImportLimits`, `FrameworkLimits`**: the bounds on files, bytes and
  relations; hitting one marks the result `partial` instead of failing.
- **`folder_tree`**: the one rule for showing structure a level at a time, shared by the canvas
  and `repo_context`.

## A worked example

```rust
use rusty_repo_model::{
    import_graph::ImportQuery,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder, RootRelativePath},
};
use tokio_util::sync::CancellationToken;

let graph = RepositoryGraphBuilder::new("/work/shop")?
    .import_support(rust_support)?          // from the rusty-lang-* crates
    .framework_adapter(spring_adapter)      // from rusty-frameworks
    .build();

let snapshot = graph.refresh(CancellationToken::new()).await?;
println!("{} packages, snapshot {}", snapshot.graph.packages.len(), snapshot.id);

// What imports src/db.rs, and in which group does it live?
let file = RootRelativePath::new("src/db.rs")?;
let importers = graph.dependents(&ImportQuery {
    snapshot: snapshot.id.clone(),
    file: file.clone(),
    item: None,
    filter: Default::default(),
    cursor: None,
    limit: 50,
})?;
let groups = graph.groups_of(&snapshot.id, &file)?;
```

A repository with ten thousand files is scanned once; a second `refresh` after one file changes
re-extracts only that file, because extraction is cached by content hash.

## Public API

- `repository_intelligence`: `RepositoryGraph`, `RepositoryGraphBuilder`,
  `RepositoryGraphService`, `RepositorySnapshot`, `RootRelativePath`, discovery and manifest
  traits, `read_contained_file`.
- `import_graph`: `ImportIndex`, `ImportSupport`, `ImportPipeline`, `ImportLimits`, query and
  result types, the language adapter traits.
- `dependency_groups`: `Group`, `GroupIndex`, `DependencyGroupAnalyzer` and the group queries.
- `framework_relations`: `FrameworkAdapter`, `FrameworkAnalyzer`, `FrameworkRelation` and queries.
- `folder_tree`: the folder-level projection.

## Errors

`DiscoveryError` (cancelled, root unreadable, limit exceeded where a partial result is not
allowed), `ImportBuildError`, `ImportQueryError`, `GroupQueryError` and `FrameworkQueryError`.
Queries refuse a cursor from a different snapshot. Bounded work reports `partial` plus a
diagnostic naming the limit; it does not fail the whole refresh.

## Design decisions

- **Contained reads.** Discovery cannot be tricked into reading outside the root; it fails closed
  on platforms without the needed system calls.
- **Everything is bounded.** Files, bytes, relations per file and in total, snapshots kept.
- **Languages and frameworks are plugins.** Adding a language is a crate, not an edit here.
- **Immutable snapshots with deterministic ids.** Queries never see a half-built state, and
  caches can be keyed by snapshot id.
- **Known exceptions to the size rules.** The `RepositoryGraph` read interface is one wide trait
  (18 methods), so the file that implements it exceeds the function-count limit; splitting the
  trait into snapshot, import, group and framework queries is planned separately because it
  changes every consumer.

## Testing

Unit tests sit beside the modules (discovery, manifests, snapshot identity, import resolution
cache, group rules, the strongly-connected-components search, framework analysis). Language
behavior is tested in the `rusty-lang-*` crates against this crate's traits. Run
`cargo test -p rusty-repo-model`.

## In the workspace

- **Depends on:** nothing else in the workspace
- **Used by:** `rusty-context-repo`, `rusty-engine`, `rusty-findings`, `rusty-frameworks`, `rusty-git-analysis`, `rusty-lang-api`, `rusty-lang-csharp`, `rusty-lang-go`, `rusty-lang-java`, `rusty-lang-native`, `rusty-lang-php`, `rusty-lang-python`, `rusty-repo-wiki`, `rusty-tool-repo`

## Must never

- Parse a programming language or know a framework; plug-in crates do.
- Follow a symlink or read outside the repository root.
- Run an agent or depend on the engine.
