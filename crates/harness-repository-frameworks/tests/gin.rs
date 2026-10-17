mod support;

use std::sync::Arc;

use harness_repository_frameworks::GinAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

const GO_MOD: &str =
    "module example.com/api\n\ngo 1.22\n\nrequire github.com/gin-gonic/gin v1.9.1\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(GinAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn routes_groups_and_handlers_resolve_across_files_and_packages() {
    let repo = repo(
        &[
            ("go.mod", GO_MOD),
            ("main.go", "package main\n\nimport (\n\t\"example.com/api/handlers\"\n\t\"github.com/gin-gonic/gin\"\n)\n\nfunc main() {\n\tr := gin.Default()\n\tr.GET(\"/ping\", ping)\n\tv1 := r.Group(\"/v1\")\n\t{\n\t\tv1.POST(\"/users\", auth(), handlers.CreateUser)\n\t\tusers := v1.Group(\"/users\")\n\t\tusers.GET(\"/:id\", func(c *gin.Context) {})\n\t}\n\tr.Any(\"/any\", handlers.Any)\n\tr.Run()\n}\n"),
            ("ping.go", "package main\n\nfunc ping() {}\nfunc auth() {}\n"),
            ("handlers/handlers.go", "package handlers\n\nfunc CreateUser() {}\nfunc Any() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /ping|main.go->ping.go".into(),
            "RouteRegisteredTo|POST /v1/users|main.go->handlers/handlers.go".into(),
            "RouteRegisteredTo|GET /v1/users/:id|main.go->main.go".into(),
            "RouteRegisteredTo|ANY /any|main.go->handlers/handlers.go".into(),
        ])
    );
    let create = repo
        .relations()
        .iter()
        .find(|r| r.label == "POST /v1/users")
        .unwrap();
    assert_eq!(create.to_symbol.as_deref(), Some("CreateUser"));
    assert_eq!(
        create.evidence.text,
        "v1.POST(\"/users\", auth(), handlers.CreateUser)"
    );
}

#[tokio::test]
async fn look_alikes_computed_paths_and_unknown_receivers_emit_nothing() {
    let repo = repo(
        &[
            ("go.mod", GO_MOD),
            ("main.go", "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc main() {\n\tr := gin.New()\n\tpath := \"/x\"\n\tr.GET(path, h)\n\tcache := newCache()\n\tcache.GET(\"/not-a-route\", h)\n\tr.GET(\"/missing-handler\")\n}\n\nfunc h() {}\nfunc newCache() *cache { return nil }\ntype cache struct{}\nfunc (c *cache) GET(a string, b func()) {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
    assert_eq!(repo.diagnostics().len(), 1, "{:?}", repo.diagnostics());
}

#[tokio::test]
async fn the_adapter_is_off_without_gin() {
    let repo = repo(
        &[
            ("go.mod", "module example.com/x\n\nrequire github.com/labstack/echo/v4 v4.11.0\n"),
            ("main.go", "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc main() {\n\tr := gin.Default()\n\tr.GET(\"/x\", h)\n}\nfunc h() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}
