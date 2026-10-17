mod support;

use std::sync::Arc;

use harness_repository_frameworks::FastApiAdapter;
use harness_workspace::framework_relations::{FrameworkAdapter, RelationKind};
use support::*;

const PYPROJECT: &str = "[project]\nname = \"api\"\ndependencies = [\"fastapi\", \"uvicorn\"]\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(FastApiAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn decorated_routes_join_router_prefixes_and_follow_depends() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("app/__init__.py", ""),
            ("app/main.py", "from fastapi import FastAPI, APIRouter, Depends\nfrom app.deps import get_db\nfrom app import auth\n\napp = FastAPI()\nrouter = APIRouter(prefix=\"/items\")\n\n@app.get(\"/health\")\nasync def health():\n    return {}\n\n@router.post(\"/\")\ndef create(db=Depends(get_db), user=Depends(auth.current_user)):\n    pass\n\n@router.get(\"/{item_id}\")\ndef read(item_id: int, db = Depends(local_dep)):\n    pass\n\n@app.api_route(\"/multi\", methods=[\"GET\", \"POST\"])\ndef multi():\n    pass\n\ndef local_dep():\n    pass\n"),
            ("app/deps.py", "def get_db():\n    yield 1\n"),
            ("app/auth.py", "def current_user():\n    pass\n"),
        ],
        adapters(),
    )
    .await;
    let routes = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("RouteRegisteredTo"))
            .collect(),
    );
    assert_eq!(
        routes,
        sorted(vec![
            "RouteRegisteredTo|GET /health|app/main.py->app/main.py".into(),
            "RouteRegisteredTo|POST /items/|app/main.py->app/main.py".into(),
            "RouteRegisteredTo|GET /items/{item_id}|app/main.py->app/main.py".into(),
            "RouteRegisteredTo|GET /multi|app/main.py->app/main.py".into(),
            "RouteRegisteredTo|POST /multi|app/main.py->app/main.py".into(),
        ])
    );
    let provided = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("ProviderInjectedInto"))
            .collect(),
    );
    assert_eq!(
        provided,
        sorted(vec![
            "ProviderInjectedInto|get_db|app/deps.py->app/main.py".into(),
            "ProviderInjectedInto|current_user|app/auth.py->app/main.py".into(),
            "ProviderInjectedInto|local_dep|app/main.py->app/main.py".into(),
        ])
    );
    let create = repo
        .relations()
        .iter()
        .find(|r| r.kind == RelationKind::RouteRegisteredTo && r.label == "POST /items/")
        .unwrap();
    assert_eq!(create.to_symbol.as_deref(), Some("create"));
    assert_eq!(create.evidence.text, "@router.post(\"/\")");
}

#[tokio::test]
async fn unprovable_receivers_and_computed_paths_are_reported_and_look_alikes_ignored() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("app/routes.py", "from fastapi import FastAPI\nfrom somewhere import router\napp = FastAPI()\nbase = '/x'\n\n@router.get(\"/imported\")\ndef a():\n    pass\n\n@app.get(base)\ndef b():\n    pass\n\n@cache.get(\"nothing\")\ndef c():\n    pass\n\n# @app.get(\"/commented\")\ndef d():\n    return \"@app.get('/in-string')\"\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
    let diagnostics = repo.diagnostics();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(diagnostics
        .iter()
        .any(|d| d.contains("not provably a FastAPI app")));
    assert!(diagnostics.iter().any(|d| d.contains("computed path")));
}

#[tokio::test]
async fn the_adapter_is_off_without_fastapi() {
    let repo = repo(
        &[
            ("pyproject.toml", "[project]\nname = \"x\"\ndependencies = [\"flask\"]\n"),
            ("a.py", "from fastapi import FastAPI\napp = FastAPI()\n@app.get(\"/x\")\ndef x():\n    pass\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}
