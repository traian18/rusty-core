mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn extracts_functions_impls_closures_and_module_owner() {
    let src = "struct S;\nimpl S {\n    fn new() -> S { S }\n    fn run(&self) { self.go(); self.go(); Self::new(); }\n    fn go(&self) {}\n}\nimpl Tr for S { fn t(&self) { self.go(); } }\nmod inner { pub fn f() { g(); } fn g() {} }\nfn top() { let f = |x| helper(x); f(1); }\nfn helper(x: i32) {}\nconst K: i32 = init();\nfn init() -> i32 { 0 }\n";
    let f = Fixture::new(&[("a.rs", src)]).await;
    assert_eq!(
        f.callable_by_qualified("a.rs", "S.new").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.rs", "S.t").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.rs", "inner.f").kind,
        CallableKind::Function
    );
    let closure = f.callable_by_qualified("a.rs", "top.f");
    assert_eq!(closure.kind, CallableKind::Closure);
    assert_eq!(closure.owner.as_ref(), Some(&f.callable("a.rs", "top").id));
    let go = f.sites("a.rs", "run", "go").await;
    assert_eq!(go.len(), 2);
    assert_ne!(go[0].span.start_byte, go[1].span.start_byte);
    assert_eq!(
        &src[go[0].span.start_byte as usize..go[0].span.end_byte as usize],
        "go"
    );
    let module = f.callable_by_qualified("a.rs", "<module>");
    assert_eq!(f.calls(module).await.len(), 1);
    // `self.go()` and `Self::new()` resolve through the enclosing impl type.
    let target = &f.callable_by_qualified("a.rs", "S.go").id;
    assert!(go
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(target)));
    f.assert_resolved_q("a.rs", "run", "new", "a.rs", "S.new")
        .await;
    f.assert_resolved_q("a.rs", "t", "go", "a.rs", "S.go").await;
    // Inline modules scope bare names: `g` is visible inside `inner`, `top` is not.
    let inner_f = f.callable_by_qualified("a.rs", "inner.f").clone();
    let calls = f.calls(&inner_f).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].target.as_ref(),
        Some(&f.callable_by_qualified("a.rs", "inner.g").id)
    );
    // The closure's call to `helper` resolves to the top-level function, not to anything in `inner`.
    let top_f = f.callable_by_qualified("a.rs", "top.f").clone();
    let calls = f.calls(&top_f).await;
    assert_eq!(
        calls[0].target.as_ref(),
        Some(&f.callable_by_qualified("a.rs", "helper").id)
    );
}

#[tokio::test]
async fn module_paths_use_groups_aliases_reexports_and_externals() {
    let main = "mod util;
mod models;
mod deep;
use crate::util::{helper, other as o};
use models::Foo;
use util::exported;
use std::collections::HashMap;
fn main() {
    helper();
    o();
    util::helper();
    crate::util::other();
    exported();
    inner::f();
    missing();
    let m = HashMap::new();
    let h = Foo { };
    h.run();
    Foo::make();
}
mod inner { pub fn f() {} }
";
    let util = "pub fn helper() {}
pub fn other() {}
pub use crate::deep::exported;
";
    let deep = "pub fn exported() {}
";
    let models = "pub struct Foo;
impl Foo {
    pub fn make() -> Foo { Foo }
    pub fn run(&self) { self.go(); super::util::helper(); }
    fn go(&self) {}
}
";
    let files = [
        ("src/main.rs", main),
        ("src/util.rs", util),
        ("src/deep.rs", deep),
        ("src/models.rs", models),
    ];
    let f = Fixture::new(&files).await;
    let m = "src/main.rs";
    f.assert_resolved(m, "main", "o", "src/util.rs", "other")
        .await;
    f.assert_resolved(m, "main", "other", "src/util.rs", "other")
        .await;
    f.assert_resolved(m, "main", "exported", "src/deep.rs", "exported")
        .await;
    f.assert_resolved_q(m, "main", "f", m, "inner.f").await;
    f.assert_resolved(m, "main", "run", "src/models.rs", "run")
        .await;
    f.assert_resolved(m, "main", "make", "src/models.rs", "make")
        .await;
    f.assert_unresolved(m, "main", "missing", "unresolved-name")
        .await;
    f.assert_external(m, "main", "new", "std::collections::HashMap")
        .await;
    // `util::helper()` and the aliased/grouped imports all name the same definition.
    let helpers = f.sites(m, "main", "helper").await;
    assert_eq!(helpers.len(), 2);
    let target = &f.callable("src/util.rs", "helper").id;
    assert!(helpers
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(target)));
    // `super::util::helper()` from models.rs.
    f.assert_resolved("src/models.rs", "run", "helper", "src/util.rs", "helper")
        .await;
    let evidence = f.site(m, "main", "exported").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "static-import");
}

#[tokio::test]
async fn shadowing_and_dynamic_negatives() {
    let src = "fn target() {}
fn plain() { target(); }
fn via_param(target: fn()) { target(); }
fn via_closure() { let target = || {}; target(); }
fn via_nested() { fn target() {} target(); }
fn dynamic(v: Vec<Box<dyn Fn()>>) { v[0](); (v.pop().unwrap())(); }
";
    let f = Fixture::new(&[("a.rs", src)]).await;
    f.assert_resolved_q("a.rs", "plain", "target", "a.rs", "target")
        .await;
    f.assert_unresolved("a.rs", "via_param", "target", "local-value-callee")
        .await;
    f.assert_resolved_q(
        "a.rs",
        "via_closure",
        "target",
        "a.rs",
        "via_closure.target",
    )
    .await;
    f.assert_resolved_q("a.rs", "via_nested", "target", "a.rs", "via_nested.target")
        .await;
    let calls = f.calls_in("a.rs", "dynamic").await;
    assert!(calls
        .iter()
        .any(|c| f.callee_text(c).starts_with("v[0]") && c.resolution == Resolution::Unresolved));
}

#[tokio::test]
async fn malformed_rust_is_partial_and_not_exact() {
    let src = "fn callee() {}
fn broken() { callee( }
fn fine() { callee(); }
";
    let f = Fixture::new(&[("a.rs", src)]).await;
    assert!(f.refresh.metadata.partial);
    f.assert_resolved("a.rs", "fine", "callee", "a.rs", "callee")
        .await;
    for caller in f.callables.iter().filter(|c| c.name == "broken") {
        for call in f.calls(caller).await {
            assert_ne!(call.resolution, Resolution::Resolved);
        }
    }
}
