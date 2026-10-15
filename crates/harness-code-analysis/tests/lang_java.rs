mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

const UTIL: &str = "package com.ex.util;\npublic class Util {\n  public static void helper() {}\n  public static void other() {}\n}\n";
const HELPER: &str = "package com.ex.util;\npublic class Helper {\n  public Helper() {}\n  public Helper(int x) {}\n  public void go() {}\n  public void go(int a) {}\n  public static void stat() {}\n}\n";
const BARE: &str = "package com.ex.util;\npublic class Bare {}\n";

#[tokio::test]
async fn extracts_constructors_methods_lambdas_and_initialisers() {
    let src = "package com.ex.app;\npublic class A {\n  int f = make();\n  A(int x) { run(); run(); }\n  void run() { Runnable r = () -> run(); r.run(); }\n  void run(int a) {}\n}\n";
    let f = Fixture::new(&[("A.java", src)]).await;
    assert_eq!(
        f.callable_by_qualified("A.java", "com.ex.app.A.A").kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable("A.java", "A").span.end_byte - f.callable("A.java", "A").span.start_byte,
        1
    );
    let runs: Vec<_> = f
        .callables
        .iter()
        .filter(|c| c.name == "run" && c.kind == CallableKind::Method)
        .collect();
    assert_eq!(runs.len(), 2);
    let lambda = f
        .callables
        .iter()
        .find(|c| c.name.starts_with("<closure@") || c.name == "r")
        .unwrap();
    assert_eq!(lambda.kind, CallableKind::Closure);
    // Field initialisers are owned by the class initialiser, not by a method.
    let init = f.callable_by_qualified("A.java", "com.ex.app.A.<init:A>");
    assert_eq!(init.kind, CallableKind::Synthetic);
    assert_eq!(f.calls(init).await.len(), 1);
    let ctor_calls = f.sites("A.java", "A", "run").await;
    assert_eq!(ctor_calls.len(), 2);
    assert_ne!(ctor_calls[0].span.start_byte, ctor_calls[1].span.start_byte);
    assert_eq!(
        &src[ctor_calls[0].span.start_byte as usize..ctor_calls[0].span.end_byte as usize],
        "run"
    );
}

#[tokio::test]
async fn overloads_stay_possible_and_this_calls_resolve() {
    let src = "public class A {\n  void go() {}\n  void go(int a) {}\n  void only() {}\n  void use() { go(); this.go(1); only(); this.only(); }\n}\n";
    let f = Fixture::new(&[("A.java", src)]).await;
    let go_calls = f.sites("A.java", "use", "go").await;
    assert_eq!(go_calls.len(), 2);
    for call in &go_calls {
        assert_eq!(
            call.resolution,
            Resolution::Possible,
            "overloads are not disambiguated"
        );
        assert_eq!(call.candidates.len(), 2);
        assert!(call.target.is_none());
    }
    let only = f.sites("A.java", "use", "only").await;
    assert!(only.iter().all(|c| c.resolution == Resolution::Resolved));
}

#[tokio::test]
async fn cross_file_imports_static_imports_and_packages() {
    let main = "package com.ex.app;\nimport com.ex.util.Helper;\nimport com.ex.lib.*;\nimport static com.ex.util.Util.helper;\nimport java.util.List;\npublic class Main {\n  void go() {\n    Helper h = new Helper();\n    h.go();\n    helper();\n    Util.other();\n    Helper.stat();\n    new Bare();\n    new Widget();\n    java.util.List<String> l = null;\n    l.add(null);\n    List.of();\n  }\n}\n";
    let lib = "package com.ex.lib;\npublic class Widget {\n  public Widget() {}\n}\n";
    let files = [
        ("com/ex/util/Util.java", UTIL),
        ("com/ex/util/Helper.java", HELPER),
        ("com/ex/util/Bare.java", BARE),
        ("com/ex/lib/Widget.java", lib),
        ("com/ex/app/Main.java", main),
    ];
    let f = Fixture::new(&files).await;
    let m = "com/ex/app/Main.java";
    // Two constructors and two `go` overloads: ambiguity is preserved, nothing is picked.
    f.assert_possible_in(m, "go", "Helper", 2, "com/ex/util/Helper.java")
        .await;
    f.assert_possible_in(m, "go", "go", 2, "com/ex/util/Helper.java")
        .await;
    f.assert_resolved(m, "go", "helper", "com/ex/util/Util.java", "helper")
        .await;
    f.assert_resolved(m, "go", "stat", "com/ex/util/Helper.java", "stat")
        .await;
    f.assert_resolved(m, "go", "Widget", "com/ex/lib/Widget.java", "Widget")
        .await;
    // `Util` is not imported (only one static member is) so it is not visible by simple name.
    f.assert_unresolved(m, "go", "other", "dynamic-receiver")
        .await;
    f.assert_unresolved(m, "go", "Bare", "unresolved-type")
        .await;
    f.assert_unresolved(m, "go", "add", "dynamic-receiver")
        .await;
    f.assert_external(m, "go", "of", "java.util.List").await;
    let evidence = f.site(m, "go", "helper").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "static-import");
    assert!(main[evidence.span.unwrap().start_byte as usize..].starts_with("import static"));
}

#[tokio::test]
async fn same_package_types_are_visible_without_imports() {
    let a = "package p;\npublic class A { void go() { new B().run(); B.make(); } }\n";
    let b = "package p;\npublic class B { public B() {} public void run() {} public static void make() {} }\n";
    let other = "package q;\npublic class C { void go() { new B(); } }\n";
    let f = Fixture::new(&[("p/A.java", a), ("p/B.java", b), ("q/C.java", other)]).await;
    f.assert_resolved("p/A.java", "go", "B", "p/B.java", "B")
        .await;
    f.assert_resolved("p/A.java", "go", "make", "p/B.java", "make")
        .await;
    // `new B().run()` has a call-result receiver: not statically typed here.
    f.assert_unresolved("p/A.java", "go", "run", "dynamic-receiver")
        .await;
    // A different package does not see B.
    f.assert_unresolved("q/C.java", "go", "B", "unresolved-type")
        .await;
}

#[tokio::test]
async fn inheritance_super_calls_and_virtual_dispatch() {
    let base = "package p;\npublic class Base { public void hello() {} public void work() {} }\n";
    let derived = "package p;\npublic class Derived extends Base {\n  public void work() { super.work(); hello(); }\n}\n";
    let user = "package p;\npublic class User {\n  void go(Base b) { b.work(); Derived d = new Derived(); d.work(); d.hello(); }\n}\n";
    let f = Fixture::new(&[
        ("p/Base.java", base),
        ("p/Derived.java", derived),
        ("p/User.java", user),
    ])
    .await;
    f.assert_resolved_q(
        "p/Derived.java",
        "work",
        "work",
        "p/Base.java",
        "p.Base.work",
    )
    .await;
    f.assert_resolved_q(
        "p/Derived.java",
        "work",
        "hello",
        "p/Base.java",
        "p.Base.hello",
    )
    .await;
    // A parameter typed Base may hold a Derived: both implementations are candidates.
    let work = f.sites("p/User.java", "go", "work").await;
    assert_eq!(work[0].resolution, Resolution::Possible);
    assert_eq!(work[0].candidates.len(), 2);
    assert_eq!(
        work[1].resolution,
        Resolution::Resolved,
        "constructed Derived is exact"
    );
}
