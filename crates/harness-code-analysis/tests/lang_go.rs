mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn extracts_functions_methods_receivers_closures_and_package_initialisers() {
    let src = "package app\n\ntype T struct{}\n\nvar registry = build()\n\nfunc build() int { return 0 }\n\nfunc (t *T) Run() { t.Go(); t.Go() }\n\nfunc (t T) Go() {}\n\nfunc top() {\n\tf := func() { top() }\n\tf()\n}\n";
    let f = Fixture::new(&[("a.go", src)]).await;
    assert_eq!(
        f.callable_by_qualified("a.go", "app.T.Run").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.go", "app.T.Go").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.go", "app.top").kind,
        CallableKind::Function
    );
    let closure = f.callable_by_qualified("a.go", "app.top.f");
    assert_eq!(closure.kind, CallableKind::Closure);
    assert_eq!(closure.owner.as_ref(), Some(&f.callable("a.go", "top").id));
    let go_calls = f.sites("a.go", "Run", "Go").await;
    assert_eq!(go_calls.len(), 2);
    assert_ne!(go_calls[0].span.start_byte, go_calls[1].span.start_byte);
    // Package-level initialisers belong to the synthetic module owner.
    let module = f.callable_by_qualified("a.go", "app.<module>");
    assert_eq!(f.calls(module).await.len(), 1);
    // The receiver variable is not a free name: it resolves through the receiver type.
    let go = &f.callable_by_qualified("a.go", "app.T.Go").id;
    assert!(go_calls
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(go)));
    f.assert_resolved("a.go", "top", "f", "a.go", "f").await;
}

#[tokio::test]
async fn shadowing_same_package_files_and_conversions() {
    let a = "package app\n\ntype Celsius float64\n\nfunc target() {}\n\nfunc plain() { target() }\n\nfunc viaParam(target func()) { target() }\n\nfunc viaLocal() { target := func() {}; target() }\n\nfunc convert() { _ = Celsius(1.5) }\n";
    let b = "package app\n\nfunc other() { target(); plain() }\n";
    let c = "package different\n\nfunc stray() { target() }\n";
    let f = Fixture::new(&[("pkg/a.go", a), ("pkg/b.go", b), ("pkg/c.go", c)]).await;
    f.assert_resolved_q("pkg/a.go", "plain", "target", "pkg/a.go", "app.target")
        .await;
    f.assert_unresolved("pkg/a.go", "viaParam", "target", "local-value-callee")
        .await;
    f.assert_resolved_q(
        "pkg/a.go",
        "viaLocal",
        "target",
        "pkg/a.go",
        "app.viaLocal.target",
    )
    .await;
    // Other files of the same directory + package share the scope.
    f.assert_resolved_q("pkg/b.go", "other", "target", "pkg/a.go", "app.target")
        .await;
    f.assert_resolved("pkg/b.go", "other", "plain", "pkg/a.go", "plain")
        .await;
    // A different package clause in the same directory does not.
    f.assert_unresolved("pkg/c.go", "stray", "target", "unresolved-name")
        .await;
    // `Celsius(1.5)` is a conversion of a workspace type, not a call.
    f.assert_unresolved("pkg/a.go", "convert", "Celsius", "type-conversion")
        .await;
}

#[tokio::test]
async fn imports_aliases_methods_embedding_and_externals() {
    let util = "package util\n\ntype Helper struct{}\n\nfunc Help() {}\n\nfunc (h *Helper) Go() {}\n\ntype Base struct{}\n\nfunc (b *Base) Inherited() {}\n\ntype Derived struct { Base }\n";
    let other = "package lib\n\nfunc X() {}\n";
    let main = "package main\n\nimport (\n\t\"fmt\"\n\tu \"example.com/app/internal/util\"\n\t\"example.com/app/lib\"\n)\n\nfunc main() {\n\tu.Help()\n\tlib.X()\n\tfmt.Println()\n\th := &u.Helper{}\n\th.Go()\n\td := u.Derived{}\n\td.Inherited()\n\tvar i interface{ Run() }\n\ti.Run()\n\tmissing.Call()\n}\n";
    let files = [
        ("internal/util/util.go", util),
        ("lib/lib.go", other),
        ("cmd/main.go", main),
    ];
    let f = Fixture::new(&files).await;
    f.assert_resolved(
        "cmd/main.go",
        "main",
        "Help",
        "internal/util/util.go",
        "Help",
    )
    .await;
    f.assert_resolved("cmd/main.go", "main", "X", "lib/lib.go", "X")
        .await;
    f.assert_external("cmd/main.go", "main", "Println", "fmt")
        .await;
    // Composite-literal typed receivers.
    f.assert_resolved_q(
        "cmd/main.go",
        "main",
        "Go",
        "internal/util/util.go",
        "util.Helper.Go",
    )
    .await;
    // Promoted method through an embedded struct.
    f.assert_resolved_q(
        "cmd/main.go",
        "main",
        "Inherited",
        "internal/util/util.go",
        "util.Base.Inherited",
    )
    .await;
    // Interface and unknown package calls stay unresolved.
    let run = f.site("cmd/main.go", "main", "Run").await;
    assert_eq!(run.resolution, Resolution::Unresolved);
    f.assert_unresolved("cmd/main.go", "main", "Call", "dynamic-receiver")
        .await;
    let evidence = f
        .site("cmd/main.go", "main", "Help")
        .await
        .evidence
        .unwrap();
    assert_eq!(evidence.rule_id, "package-import");
    assert!(main[evidence.span.unwrap().start_byte as usize..].starts_with("u \"example.com"));
}

#[tokio::test]
async fn malformed_go_is_partial() {
    let src = "package app\n\nfunc callee() {}\n\nfunc broken( {\n\tcallee()\n}\n\nfunc fine() { callee() }\n";
    let f = Fixture::new(&[("a.go", src)]).await;
    assert!(f.refresh.metadata.partial);
    f.assert_resolved("a.go", "fine", "callee", "a.go", "callee")
        .await;
    for caller in f.callables.iter().filter(|c| c.name == "broken") {
        for call in f.calls(caller).await {
            assert_ne!(call.resolution, Resolution::Resolved);
        }
    }
}
