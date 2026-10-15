mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

const HEADER: &str = "#pragma once\nnamespace ns { void f(); }\nclass Foo {\npublic:\n  Foo();\n  Foo(int x);\n  void m();\n  static void stat();\n};\nvoid helper();\n";
const SOURCE: &str = "#include \"util.h\"\nFoo::Foo() {}\nFoo::Foo(int x) {}\nvoid Foo::m() { helper(); }\nvoid Foo::stat() {}\nvoid helper() {}\nnamespace ns { void f() {} }\n";

#[tokio::test]
async fn extracts_members_constructors_lambdas_namespaces_and_owners() {
    let src = "namespace outer {\n  void g() {}\n  namespace inner { void h() { g(); g(); } }\n}\nclass K {\npublic:\n  K() { b(); }\n  void a() { b(); this->b(); auto l = [](int q) { return q; }; l(1); }\n  void b() {}\n  int field = make();\n};\nint make() { return 0; }\nint global = make();\n";
    let f = Fixture::new(&[("a.cpp", src)]).await;
    assert_eq!(
        f.callable_by_qualified("a.cpp", "outer.g").kind,
        CallableKind::Function
    );
    assert_eq!(
        f.callable_by_qualified("a.cpp", "outer::inner.h").kind,
        CallableKind::Function
    );
    assert_eq!(
        f.callable_by_qualified("a.cpp", "K.K").kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable_by_qualified("a.cpp", "K.a").kind,
        CallableKind::Method
    );
    let lambda = f.callable_by_qualified("a.cpp", "K.a.l");
    assert_eq!(lambda.kind, CallableKind::Closure);
    assert_eq!(lambda.owner.as_ref(), Some(&f.callable("a.cpp", "a").id));
    let gs = f.sites("a.cpp", "h", "g").await;
    assert_eq!(gs.len(), 2);
    assert_ne!(gs[0].span.start_byte, gs[1].span.start_byte);
    // Inner namespaces see enclosing-namespace functions.
    let g = &f.callable("a.cpp", "g").id;
    assert!(gs
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(g)));
    // Bare and `this->` calls inside a class resolve to its members.
    let bs = f.sites("a.cpp", "a", "b").await;
    assert_eq!(bs.len(), 2);
    let b = &f.callable("a.cpp", "b").id;
    assert!(bs
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(b)));
    f.assert_resolved_q("a.cpp", "a", "l", "a.cpp", "K.a.l")
        .await;
    // Field initialisers and globals have synthetic owners.
    let init = f.callable_by_qualified("a.cpp", "K.<init:K>");
    assert_eq!(f.calls(init).await.len(), 1);
    let module = f.callable_by_qualified("a.cpp", "<module>");
    assert_eq!(f.calls(module).await.len(), 1);
}

#[tokio::test]
async fn header_and_companion_source_resolution() {
    let main = "#include \"util.h\"\n#include <vector>\nvoid go() {\n  Foo a;\n  Foo b(1);\n  b.m();\n  Foo::stat();\n  helper();\n  ns::f();\n  new Foo();\n  std::sort();\n  unknownFn();\n}\n";
    let f = Fixture::new(&[("util.h", HEADER), ("util.cpp", SOURCE), ("main.cpp", main)]).await;
    let m = "main.cpp";
    // `Foo b(1)` and `new Foo()` construct a Foo; two constructor definitions stay ambiguous.
    let foos = f.sites(m, "go", "Foo").await;
    assert_eq!(foos.len(), 2);
    for site in &foos {
        assert_eq!(site.resolution, Resolution::Possible);
        assert_eq!(site.candidates.len(), 2);
        assert!(site.target.is_none());
    }
    // `b` is exactly a Foo, so `m` resolves to the out-of-line definition in the companion source.
    f.assert_resolved("util.cpp", "m", "helper", "util.cpp", "helper")
        .await;
    f.assert_resolved(m, "go", "m", "util.cpp", "m").await;
    f.assert_resolved(m, "go", "stat", "util.cpp", "stat").await;
    f.assert_resolved(m, "go", "helper", "util.cpp", "helper")
        .await;
    f.assert_resolved(m, "go", "f", "util.cpp", "f").await;
    f.assert_unresolved(m, "go", "sort", "dynamic-receiver")
        .await;
    f.assert_unresolved(m, "go", "unknownFn", "unresolved-name")
        .await;
    let evidence = f.site(m, "go", "helper").await.evidence.unwrap();
    assert!(
        ["include", "companion-file"].contains(&evidence.rule_id.as_str()),
        "{evidence:?}"
    );
}

#[tokio::test]
async fn files_without_an_include_do_not_see_each_other() {
    let a = "void helper() {}\n";
    let b = "void go() { helper(); }\n";
    let f = Fixture::new(&[("a.cpp", a), ("b.cpp", b)]).await;
    f.assert_unresolved("b.cpp", "go", "helper", "unresolved-name")
        .await;
    let c = "#include \"a.cpp\"\nvoid go2() { helper(); }\n";
    let f = Fixture::new(&[("a.cpp", a), ("c.cpp", c)]).await;
    f.assert_resolved("c.cpp", "go2", "helper", "a.cpp", "helper")
        .await;
}

#[tokio::test]
async fn shadowing_overloads_using_and_preprocessor_branches() {
    let src = "#include <functional>\nvoid target() {}\nvoid plain() { target(); }\nvoid viaParam(std::function<void()> target) { target(); }\nvoid viaLocal() { auto target = []() {}; target(); }\nvoid over(int) {}\nvoid over(double) {}\nvoid callOver() { over(1); }\n#ifdef FAST\nvoid pick() {}\n#else\nvoid pick() {}\n#endif\nvoid callPick() { pick(); }\nnamespace lib { void util() {} }\nusing namespace lib;\nvoid viaUsing() { util(); }\nvoid dyn(void (*fp)()) { fp(); (*fp)(); }\n";
    let f = Fixture::new(&[("a.cpp", src)]).await;
    f.assert_resolved_q("a.cpp", "plain", "target", "a.cpp", "target")
        .await;
    f.assert_unresolved("a.cpp", "viaParam", "target", "local-value-callee")
        .await;
    f.assert_resolved_q("a.cpp", "viaLocal", "target", "a.cpp", "viaLocal.target")
        .await;
    f.assert_possible_in("a.cpp", "callOver", "over", 2, "a.cpp")
        .await;
    f.assert_possible_in("a.cpp", "callPick", "pick", 2, "a.cpp")
        .await;
    f.assert_resolved("a.cpp", "viaUsing", "util", "a.cpp", "util")
        .await;
    f.assert_unresolved("a.cpp", "dyn", "fp", "local-value-callee")
        .await;
}

#[tokio::test]
async fn inheritance_and_malformed_cpp() {
    let src = "class Base { public: void hello() {} virtual void work() {} };\nclass Derived : public Base {\npublic:\n  void work() {}\n  void run() { hello(); }\n};\nvoid use() { Derived d; d.work(); d.hello(); Derived* p = new Derived(); p->work(); }\n";
    let f = Fixture::new(&[("a.cpp", src)]).await;
    f.assert_resolved_q("a.cpp", "run", "hello", "a.cpp", "Base.hello")
        .await;
    let works = f.sites("a.cpp", "use", "work").await;
    assert_eq!(works.len(), 2);
    let derived_work = &f.callable_by_qualified("a.cpp", "Derived.work").id;
    assert!(works
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(derived_work)));
    f.assert_resolved_q("a.cpp", "use", "hello", "a.cpp", "Base.hello")
        .await;

    let broken = "void callee() {}\nvoid bad() { callee( }\nvoid fine() { callee(); }\n";
    let f = Fixture::new(&[("b.cpp", broken)]).await;
    assert!(f.refresh.metadata.partial);
    f.assert_resolved("b.cpp", "fine", "callee", "b.cpp", "callee")
        .await;
    for caller in f.callables.iter().filter(|c| c.name == "bad") {
        for call in f.calls(caller).await {
            assert_ne!(call.resolution, Resolution::Resolved);
        }
    }
}
