mod support;

use std::sync::Arc;

use harness_repository_frameworks::NetHttpAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

const GO_MOD: &str = "module example.com/svc\n\ngo 1.22\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(NetHttpAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn default_mux_and_new_serve_mux_registrations_are_read() {
    let repo = repo(
        &[
            ("go.mod", GO_MOD),
            ("main.go", "package main\n\nimport (\n\t\"net/http\"\n\n\t\"example.com/svc/api\"\n)\n\nfunc main() {\n\thttp.HandleFunc(\"/\", home)\n\tmux := http.NewServeMux()\n\tmux.HandleFunc(\"GET /items/{id}\", api.Item)\n\tmux.Handle(\"/files/\", http.HandlerFunc(files))\n\tpattern := \"/dyn\"\n\tmux.HandleFunc(pattern, home)\n\thttp.ListenAndServe(\":8080\", mux)\n}\n\nfunc home(w http.ResponseWriter, r *http.Request) {}\nfunc files(w http.ResponseWriter, r *http.Request) {}\n"),
            ("api/item.go", "package api\n\nimport \"net/http\"\n\nfunc Item(w http.ResponseWriter, r *http.Request) {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|ANY /|main.go->main.go".into(),
            "RouteRegisteredTo|GET /items/{id}|main.go->api/item.go".into(),
            "RouteRegisteredTo|ANY /files/|main.go->main.go".into(),
        ])
    );
    assert_eq!(repo.diagnostics().len(), 1, "{:?}", repo.diagnostics());
    assert!(repo.diagnostics()[0].contains("computed pattern"));
}

#[tokio::test]
async fn files_without_net_http_and_non_mux_receivers_are_ignored() {
    let repo = repo(
        &[
            ("go.mod", GO_MOD),
            ("a.go", "package main\n\ntype router struct{}\n\nfunc (r router) HandleFunc(p string, h func()) {}\n\nfunc setup(r router) {\n\tr.HandleFunc(\"/x\", func() {})\n}\n"),
            ("b.go", "package main\n\nimport \"net/http\"\n\nfunc setup(r router) {\n\tr.HandleFunc(\"/y\", func() {})\n\t_ = http.StatusOK\n}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());

    let not_go = self::not_go().await;
    assert!(not_go.relations().is_empty());
    assert!(not_go.snapshot.frameworks.applications.is_empty());
}

async fn not_go() -> Repo {
    repo(
        &[
            ("package.json", "{\"name\":\"x\"}"),
            ("a.ts", "export {}\n"),
        ],
        adapters(),
    )
    .await
}
