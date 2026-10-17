mod support;

use std::sync::Arc;

use harness_repository_frameworks::AxumAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(AxumAdapter)]
}

fn cargo(dependencies: &str) -> String {
    format!("[package]\nname = \"svc\"\nversion = \"0.1.0\"\n[dependencies]\n{dependencies}\n")
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn method_routers_resolve_their_handlers_through_modules_and_imports() {
    let repo = repo(
        &[
            ("Cargo.toml", &cargo("axum = \"0.7\"")),
            ("src/main.rs", "mod handlers;\nmod users;\nuse axum::{routing::{get, post}, Router};\nuse users::list_users;\nasync fn health() {}\nfn app() -> Router {\n    Router::new()\n        .route(\"/health\", get(health))\n        .route(\"/users\", get(list_users).post(handlers::create_user))\n        .route(\"/ping\", axum::routing::get(|| async { \"pong\" }))\n}\nfn main() {}\n"),
            ("src/handlers.rs", "pub async fn create_user() {}\n"),
            ("src/users.rs", "pub async fn list_users() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /health|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|GET /users|src/main.rs->src/users.rs".into(),
            "RouteRegisteredTo|POST /users|src/main.rs->src/handlers.rs".into(),
            "RouteRegisteredTo|GET /ping|src/main.rs->src/main.rs".into(),
        ])
    );
    let post = repo
        .relations()
        .iter()
        .find(|r| r.label == "POST /users")
        .unwrap();
    assert_eq!(post.to_symbol.as_deref(), Some("create_user"));
    assert!(
        post.evidence.text.starts_with(".route(\"/users\""),
        "{}",
        post.evidence.text
    );
}

#[tokio::test]
async fn computed_paths_and_unfamiliar_routers_are_reported_not_guessed() {
    let repo = repo(
        &[
            ("Cargo.toml", &cargo("axum = \"0.7\"")),
            ("src/main.rs", "use axum::{routing::get, Router};\nasync fn h() {}\nfn app(prefix: &str) -> Router {\n    Router::new()\n        .route(prefix, get(h))\n        .route(\"/svc\", get_service(h))\n        .route(\"/on\", on(MethodFilter::GET, h))\n}\nfn main() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
    assert_eq!(repo.diagnostics().len(), 3, "{:?}", repo.diagnostics());
}

#[tokio::test]
async fn files_that_do_not_use_axum_and_packages_without_it_are_ignored() {
    let other = repo(
        &[
            ("Cargo.toml", &cargo("axum = \"0.7\"\nsomething = \"1\"")),
            ("src/lib.rs", "struct Table;\nimpl Table { fn route(&self, _: &str, _: u8) {} }\nfn f(t: Table) { t.route(\"/x\", 1); }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(other.relations().is_empty());

    let off = repo(
        &[
            ("Cargo.toml", &cargo("actix-web = \"4\"")),
            ("src/main.rs", "use axum::{routing::get, Router};\nasync fn h() {}\nfn main() { let _ = Router::new().route(\"/x\", get(h)); }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(off.relations().is_empty());
    assert!(off.snapshot.frameworks.applications.is_empty());
}
