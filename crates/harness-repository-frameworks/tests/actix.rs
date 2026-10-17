mod support;

use std::sync::Arc;

use harness_repository_frameworks::ActixAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(ActixAdapter)]
}

fn cargo(dependencies: &str) -> String {
    format!("[package]\nname = \"svc\"\nversion = \"0.1.0\"\n[dependencies]\n{dependencies}\n")
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn attribute_routes_and_route_registrations_are_read() {
    let repo = repo(
        &[
            ("Cargo.toml", &cargo("actix-web = \"4\"")),
            ("src/main.rs", "mod api;\nuse actix_web::{get, post, route, web, App, HttpServer};\n#[get(\"/health\")]\nasync fn health() {}\n#[post(\"/items\")]\nasync fn create() {}\n#[route(\"/any\", method = \"GET\", method = \"POST\")]\nasync fn any() {}\n#[get(path)]\nasync fn computed() {}\nasync fn plain() {}\nfn configure(cfg: &mut web::ServiceConfig) {\n    cfg.route(\"/plain\", web::get().to(plain));\n    cfg.route(\"/api\", web::post().to(api::submit));\n}\nfn main() {}\n"),
            ("src/api.rs", "pub async fn submit() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /health|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|POST /items|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|GET /any|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|POST /any|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|GET /plain|src/main.rs->src/main.rs".into(),
            "RouteRegisteredTo|POST /api|src/main.rs->src/api.rs".into(),
        ])
    );
    let health = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /health")
        .unwrap();
    assert_eq!(health.to_symbol.as_deref(), Some("health"));
    assert_eq!(health.evidence.text, "#[get(\"/health\")]");
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("no literal path")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn scopes_are_reported_and_other_files_and_packages_are_ignored() {
    let repo = repo(
        &[
            ("Cargo.toml", &cargo("actix-web = \"4\"")),
            ("src/main.rs", "use actix_web::{web, App};\nasync fn h() {}\nfn main() {\n    let _ = App::new().service(web::scope(\"/v1\").route(\"/a\", web::get().to(h)));\n}\n"),
            ("src/other.rs", "#[get(\"/not-actix\")]\nfn f() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(
        repo.diagnostics().iter().any(|d| d.contains("web::scope")),
        "{:?}",
        repo.diagnostics()
    );
    assert!(repo.relations().iter().all(|r| r.label != "GET /not-actix"));

    let off = repo_off().await;
    assert!(off.relations().is_empty());
}

async fn repo_off() -> Repo {
    repo(
        &[
            ("Cargo.toml", &cargo("axum = \"0.7\"")),
            (
                "src/main.rs",
                "use actix_web::get;\n#[get(\"/x\")]\nasync fn x() {}\n",
            ),
        ],
        adapters(),
    )
    .await
}
