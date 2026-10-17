mod support;

use std::sync::Arc;

use harness_repository_frameworks::EchoAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

const GO_MOD: &str =
    "module example.com/api\n\ngo 1.22\n\nrequire github.com/labstack/echo/v4 v4.11.0\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(EchoAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn routes_and_groups_name_the_handler_after_the_path() {
    let repo = repo(
        &[
            ("go.mod", GO_MOD),
            ("main.go", "package main\n\nimport (\n\t\"example.com/api/users\"\n\t\"github.com/labstack/echo/v4\"\n)\n\nfunc main() {\n\te := echo.New()\n\te.GET(\"/health\", health)\n\tg := e.Group(\"/api\")\n\tg.POST(\"/users\", users.Create, mw)\n\tg.DELETE(\"/users/:id\", func(c echo.Context) error { return nil })\n\te.Start(\":1323\")\n}\n\nfunc health(c echo.Context) error { return nil }\nfunc mw() {}\n"),
            ("users/users.go", "package users\n\nfunc Create() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /health|main.go->main.go".into(),
            "RouteRegisteredTo|POST /api/users|main.go->users/users.go".into(),
            "RouteRegisteredTo|DELETE /api/users/:id|main.go->main.go".into(),
        ])
    );
    let create = repo
        .relations()
        .iter()
        .find(|r| r.label == "POST /api/users")
        .unwrap();
    assert_eq!(
        create.to_symbol.as_deref(),
        Some("Create"),
        "middleware after the handler is ignored"
    );
}

#[tokio::test]
async fn the_adapter_is_off_without_echo_and_ignores_other_receivers() {
    let off = repo(
        &[
            ("go.mod", "module example.com/x\n\nrequire github.com/gin-gonic/gin v1.9.1\n"),
            ("main.go", "package main\n\nimport \"github.com/labstack/echo/v4\"\n\nfunc main() {\n\te := echo.New()\n\te.GET(\"/x\", h)\n}\nfunc h() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(off.relations().is_empty());

    let other = repo(
        &[
            ("go.mod", GO_MOD),
            ("main.go", "package main\n\nimport \"github.com/labstack/echo/v4\"\n\nfunc main() {\n\t_ = echo.New()\n\tclient := newClient()\n\tclient.GET(\"/remote\", h)\n}\nfunc h() {}\nfunc newClient() *c { return nil }\ntype c struct{}\nfunc (x *c) GET(a string, b func()) {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(other.relations().is_empty());
}
