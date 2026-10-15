mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

const LIB: &str = "export function helper() {}\nexport default function dflt() {}\nexport class Widget {\n  constructor() {}\n  render() { this.draw(); }\n  draw() {}\n  static make() {}\n}\nfunction hidden() {}\nexport { hidden as visible };\n";

#[tokio::test]
async fn extracts_callables_with_ownership_and_exact_spans() {
    let main = "import { helper } from './lib.js';\nconst arrow = (x) => helper(x);\nfunction outer() {\n  function inner() { helper(); }\n  const closure = function () { inner(); };\n  inner();\n  helper();\n  helper();\n}\nclass K {\n  constructor() { outer(); }\n  m() { return () => outer(); }\n}\nouter();\n";
    let f = Fixture::new(&[("lib.js", LIB), ("main.js", main)]).await;
    for (name, kind) in [
        ("outer", CallableKind::Function),
        ("inner", CallableKind::Function),
        ("arrow", CallableKind::Closure),
        ("closure", CallableKind::Closure),
        ("constructor", CallableKind::Constructor),
        ("m", CallableKind::Method),
    ] {
        let symbol = f.callable("main.js", name);
        assert_eq!(symbol.kind, kind, "{name}");
        assert_eq!(
            &main[symbol.span.start_byte as usize..symbol.span.end_byte as usize],
            if name == "arrow" || name == "closure" {
                &main[symbol.span.start_byte as usize..symbol.span.end_byte as usize]
            } else {
                name
            },
        );
    }
    // Lexical ownership: closures and nested functions are owned by the enclosing callable.
    let inner = f.callable("main.js", "inner");
    assert_eq!(
        inner.owner.as_ref(),
        Some(&f.callable("main.js", "outer").id)
    );
    assert_eq!(
        f.callable_by_qualified("main.js", "K.m")
            .owner
            .as_ref()
            .map(|o| o.0.as_str()),
        Some("main.js:owner:K")
    );
    let anonymous: Vec<_> = f
        .callables
        .iter()
        .filter(|c| c.span.path == "main.js" && c.name.starts_with("<closure@"))
        .collect();
    assert_eq!(anonymous.len(), 1);
    let closure_in_m = anonymous[0];
    assert_eq!(
        closure_in_m.owner.as_ref(),
        Some(&f.callable("main.js", "m").id)
    );
    // Repeated calls to the same callee keep distinct spans.
    let helper_calls = f.sites("main.js", "outer", "helper").await;
    assert_eq!(helper_calls.len(), 2);
    assert_ne!(
        helper_calls[0].span.start_byte,
        helper_calls[1].span.start_byte
    );
    // Top-level calls are owned by a synthetic module owner, not a nearby function.
    let module = f.callable_by_qualified("main.js", "<module>");
    assert_eq!(module.kind, CallableKind::Synthetic);
    assert_eq!(f.calls(module).await.len(), 1);
}

#[tokio::test]
async fn resolves_same_file_calls_with_lexical_shadowing() {
    let src = "function target() {}\nfunction plain() { target(); }\nfunction viaParam(target) { target(); }\nfunction viaLocal() { const target = () => {}; target(); }\nfunction viaNested() { function target() {} target(); }\nfunction hoisted() { target(); function target() {} }\n";
    let f = Fixture::new(&[("a.js", src)]).await;
    f.assert_resolved_q("a.js", "plain", "target", "a.js", "target")
        .await;
    f.assert_unresolved("a.js", "viaParam", "target", "local-value-callee")
        .await;
    // Locals shadow the top-level function: the call binds to the nested callable instead.
    f.assert_resolved_q("a.js", "viaLocal", "target", "a.js", "viaLocal.target")
        .await;
    f.assert_resolved_q("a.js", "viaNested", "target", "a.js", "viaNested.target")
        .await;
    f.assert_resolved_q("a.js", "hoisted", "target", "a.js", "hoisted.target")
        .await;
}

#[tokio::test]
async fn resolves_cross_file_imports_exports_aliases_and_namespaces() {
    let main = "import dflt, { helper, visible as v, Widget } from './lib.js';\nimport * as lib from './lib.js';\nimport { missing } from './lib.js';\nimport leftPad from 'left-pad';\nfunction go() {\n  helper();\n  dflt();\n  v();\n  lib.helper();\n  new Widget();\n  Widget.make();\n  missing();\n  leftPad();\n}\n";
    let f = Fixture::new(&[("lib.js", LIB), ("main.js", main)]).await;
    let helpers = f.sites("main.js", "go", "helper").await;
    assert_eq!(helpers.len(), 2, "direct and namespace-qualified call");
    let helper_id = &f.callable("lib.js", "helper").id;
    assert!(helpers
        .iter()
        .all(|site| site.target.as_ref() == Some(helper_id)));
    f.assert_resolved("main.js", "go", "dflt", "lib.js", "dflt")
        .await;
    f.assert_resolved("main.js", "go", "v", "lib.js", "hidden")
        .await;
    f.assert_resolved("main.js", "go", "Widget", "lib.js", "constructor")
        .await;
    f.assert_resolved("main.js", "go", "make", "lib.js", "make")
        .await;
    f.assert_unresolved("main.js", "go", "missing", "unresolved-name")
        .await;
    f.assert_external("main.js", "go", "leftPad", "left-pad")
        .await;
    // Evidence names the import that supports the edge.
    let evidence = helpers[0].evidence.clone().unwrap();
    assert_eq!(evidence.rule_id, "static-import");
    let span = evidence.span.expect("import evidence span");
    assert_eq!(span.path, "main.js");
    assert!(main[span.start_byte as usize..span.end_byte as usize].starts_with("import dflt"));
}

#[tokio::test]
async fn namespace_import_member_and_calls_to_index() {
    let main = "import * as lib from './lib';\nfunction go() { lib.helper(); }\n";
    let f = Fixture::new(&[("lib.js", LIB), ("main.js", main)]).await;
    f.assert_resolved("main.js", "go", "helper", "lib.js", "helper")
        .await;
    let incoming = f.callers_of(f.callable("lib.js", "helper")).await;
    assert_eq!(incoming.len(), 1);
}

#[tokio::test]
async fn methods_constructors_and_this_calls() {
    let src = "class A {\n  run() { this.go(); this.missing(); }\n  go() {}\n}\nclass B extends A {\n  go() {}\n  other() { super.go(); }\n}\nfunction use() { const a = new A(); a.go(); const b = new B(); b.go(); b.run(); }\n";
    let f = Fixture::new(&[("a.js", src)]).await;
    // `this.go()` may run A.go or the overriding B.go.
    let site = f.site("a.js", "run", "go").await;
    assert_eq!(site.resolution, Resolution::Possible);
    assert_eq!(site.candidates.len(), 2);
    f.assert_unresolved("a.js", "run", "missing", "method-not-in-type")
        .await;
    // Constructed receivers have an exact dynamic type.
    let a_go = f.sites("a.js", "use", "go").await;
    assert_eq!(a_go.len(), 2);
    assert_eq!(a_go[0].resolution, Resolution::Resolved);
    assert_eq!(a_go[1].resolution, Resolution::Resolved);
    assert_ne!(a_go[0].target, a_go[1].target);
    // Inherited method found through the base class.
    let run = f.site("a.js", "use", "run").await;
    assert_eq!(run.resolution, Resolution::Resolved);
    // Implicit constructors are not invented.
    let ctor = f.sites("a.js", "use", "A").await;
    assert_eq!(ctor[0].resolution, Resolution::Unresolved);
    assert_eq!(
        ctor[0].evidence.as_ref().unwrap().rule_id,
        "implicit-constructor"
    );
    f.assert_resolved_q("a.js", "other", "go", "a.js", "A.go")
        .await;
}
