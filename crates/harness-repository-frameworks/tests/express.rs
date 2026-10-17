mod support;

use std::sync::Arc;

use harness_repository_frameworks::ExpressAdapter;
use harness_workspace::framework_relations::{Certainty, RelationKind};
use support::*;

const PACKAGE: &str = r#"{"name":"api","dependencies":{"express":"4"}}"#;

fn adapters() -> Vec<Arc<dyn harness_workspace::framework_relations::FrameworkAdapter>> {
    vec![Arc::new(ExpressAdapter)]
}

#[tokio::test]
async fn literal_routes_resolve_their_handlers_through_imports() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/app.ts", "import express from 'express';\nimport { listUsers } from './users';\nimport * as orders from './orders';\nconst app = express();\nconst router = express.Router();\napp.get('/users', listUsers);\napp.post('/inline', (req, res) => res.send('x'));\nrouter.delete('/orders/:id', auth, orders.remove);\nfunction local() {}\napp.put(['/a', '/b'], local);\nexport default app;\n"),
            ("src/users.ts", "export function listUsers() {}\n"),
            ("src/orders.ts", "export function remove() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        repo.summary(),
        [
            "RouteRegisteredTo|GET /users|src/app.ts->src/users.ts",
            "RouteRegisteredTo|POST /inline|src/app.ts->src/app.ts",
            "RouteRegisteredTo|DELETE /orders/:id|src/app.ts->src/orders.ts",
            "RouteRegisteredTo|PUT /a|src/app.ts->src/app.ts",
            "RouteRegisteredTo|PUT /b|src/app.ts->src/app.ts",
        ]
    );
    let first = &repo.relations()[0];
    assert_eq!(first.kind, RelationKind::RouteRegisteredTo);
    assert_eq!(first.certainty, Certainty::Declared);
    assert_eq!(first.rule.id, "express-route");
    assert_eq!(first.to_symbol.as_deref(), Some("listUsers"));
    assert_eq!(first.evidence.text, "app.get('/users', listUsers)");
    assert_eq!(repo.relations()[2].to_symbol.as_deref(), Some("remove"));
    assert!(repo.diagnostics().is_empty(), "{:?}", repo.diagnostics());
}

#[tokio::test]
async fn look_alikes_are_not_routes() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/lib.ts", "import express from 'express';\nconst cache = new Map();\ncache.get('/users');\nconst app = express();\napp.get('setting');\n// app.get('/commented', handler)\nconst text = \"app.get('/in-string', h)\";\nexport { app };\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
}

#[tokio::test]
async fn unprovable_receivers_and_computed_paths_are_reported_and_emit_nothing() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/routes.ts", "import express from 'express';\nexport function mount(app) {\n  app.get('/from-param', (req, res) => {});\n}\nconst app = express();\nconst base = '/x';\napp.get(base, (req, res) => {});\napp.get(`/t/${base}`, (req, res) => {});\napp.get(['/ok', base], (req, res) => {});\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    let diagnostics = repo.diagnostics();
    assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
    assert!(diagnostics
        .iter()
        .any(|d| d.contains("not provably an Express app")));
    assert!(
        diagnostics
            .iter()
            .filter(|d| d.contains("computed"))
            .count()
            == 3
    );
}

#[tokio::test]
async fn the_adapter_is_off_where_express_is_not_declared() {
    let repo = repo(
        &[
            ("package.json", r#"{"name":"plain","dependencies":{"koa":"2"}}"#),
            ("src/app.ts", "const express = () => ({ get() {} });\nconst app = express();\napp.get('/users', () => {});\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}

#[tokio::test]
async fn capability_text_names_what_is_not_read() {
    let repo = repo(&[("package.json", PACKAGE), ("a.ts", "")], adapters()).await;
    let capability = &repo.snapshot.frameworks.capabilities[0];
    assert_eq!(capability.framework, "express");
    assert!(capability
        .unsupported
        .iter()
        .any(|u| u.contains("mount prefixes")));
}
