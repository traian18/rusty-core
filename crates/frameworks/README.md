# rusty-frameworks

Framework adapters for the repository graph: routes, injected providers, rendered components and
exposed commands, read from source and manifests without running anything. Each adapter implements
`rusty_repo_model::framework_relations::FrameworkAdapter`.

## Why it exists

Imports do not show that `GET /users` is served by a function, that a service is injected into a
controller, or that a Tauri command is callable from the front end. Frameworks wire code together
through conventions, and a graph that ignores them under-reports what a change affects. Adapters
add those relations, with evidence, and say plainly what they do not read.

## How it works

1. An adapter runs only for packages that declare its framework (Axum, Actix, Tauri, Spring,
   ASP.NET, Django, Flask, FastAPI, Laravel, Symfony, Express, NestJS, Next.js, Gin, Echo, net/http,
   Angular, React, Vue).
2. It reads source with tree-sitter and, where needed, manifests and route files (`routes`,
   `handlers`, `decorators`, `go_routes`, `php_common`, `py_common`, `rust_items` are shared helpers).
3. A literal registration becomes a relation with evidence; a computed or dynamic one becomes a
   diagnostic and no relation.
4. `parallel` runs adapters across files.

## Key types and concepts

- **Relation**: framework-level edge (route to handler, provider to consumer, component to child).
- **Capability text**: each adapter states what it reads and what it leaves alone.
- **Diagnostic**: the record of a registration it could not follow.

## A worked example

```text
#[get("/users/{id}")] async fn show(...)   -> relation: route GET /users/{id} -> show
Route::get($path, $handler)                -> $path is computed: diagnostic, no relation
```

## Public API

- One adapter type per framework, and the helper modules for routes and handlers.

## Errors

Adapters do not fail a build: a file they cannot parse is a diagnostic.

## Design decisions

- **Literals only.** A guessed route is worse than a missing one.
- **Declared frameworks only**, so a package pays nothing for frameworks it does not use.

## Testing

Each adapter has fixture tests with a real-looking small project. Run `cargo test -p rusty-frameworks`.

## In the workspace

- **Depends on:** `rusty-repo-model`
- **Used by:** nothing else in the workspace

## Must never

- Run the framework or its tooling.
- Create a relation from a computed value.
- Run for a package that does not declare the framework.
