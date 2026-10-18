mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

const USER: &str = "package com.ex.model\n\nclass User(val name: String) {\n    fun greet(): String {\n        return name\n    }\n}\n";
const FORMAT: &str = "package com.ex.util\n\nfun format(value: String): String {\n    return value\n}\n\nobject Registry {\n    fun find() {\n    }\n}\n\nclass Factory {\n    companion object {\n        fun create(): Factory {\n            return Factory()\n        }\n    }\n}\n";
const LEGACY: &str = "package com.ex.legacy;\n\npublic class Legacy {\n  public Legacy() {}\n  public void load() {}\n}\n";
const APP: &str = "package com.ex.app\n\nimport com.ex.model.User\nimport com.ex.util.*\nimport com.ex.util.format as fmt\nimport com.ex.legacy.Legacy\nimport org.slf4j.LoggerFactory\n\nclass App {\n    fun run() {\n        val user = User(\"a\")\n        user.greet()\n        fmt(\"x\")\n        Registry.find()\n        Factory.create()\n        val legacy = Legacy()\n        legacy.load()\n        helper()\n        this.helper()\n        LoggerFactory.getLogger(\"x\")\n        listOf(1).map { it + 1 }\n    }\n\n    private fun helper() {\n    }\n}\n";

fn files() -> [(&'static str, &'static str); 4] {
    [
        ("com/ex/model/User.kt", USER),
        ("com/ex/util/Format.kt", FORMAT),
        ("com/ex/legacy/Legacy.java", LEGACY),
        ("com/ex/app/App.kt", APP),
    ]
}

#[tokio::test]
async fn extracts_functions_methods_constructors_objects_and_lambdas() {
    let f = Fixture::new(&files()).await;
    assert_eq!(
        f.callable_by_qualified("com/ex/util/Format.kt", "com.ex.util.format")
            .kind,
        CallableKind::Function
    );
    assert_eq!(
        f.callable_by_qualified("com/ex/model/User.kt", "com.ex.model.User.greet")
            .kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("com/ex/model/User.kt", "com.ex.model.User.User")
            .kind,
        CallableKind::Constructor,
        "the primary constructor is a callable"
    );
    assert_eq!(
        f.callable_by_qualified("com/ex/util/Format.kt", "com.ex.util.Registry.find")
            .kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("com/ex/util/Format.kt", "com.ex.util.Factory.create")
            .kind,
        CallableKind::Method,
        "companion members belong to the enclosing class"
    );
    assert!(f
        .callables
        .iter()
        .any(|c| c.span.path == "com/ex/app/App.kt" && c.kind == CallableKind::Closure));
}

#[tokio::test]
async fn imports_aliases_wildcards_objects_companions_and_java_classes_resolve() {
    let f = Fixture::new(&files()).await;
    let app = "com/ex/app/App.kt";
    f.assert_resolved(app, "run", "User", "com/ex/model/User.kt", "User")
        .await;
    f.assert_resolved(app, "run", "greet", "com/ex/model/User.kt", "greet")
        .await;
    f.assert_resolved(app, "run", "fmt", "com/ex/util/Format.kt", "format")
        .await;
    f.assert_resolved(app, "run", "find", "com/ex/util/Format.kt", "find")
        .await;
    f.assert_resolved(app, "run", "create", "com/ex/util/Format.kt", "create")
        .await;
    f.assert_resolved(app, "run", "Legacy", "com/ex/legacy/Legacy.java", "Legacy")
        .await;
    f.assert_resolved(app, "run", "load", "com/ex/legacy/Legacy.java", "load")
        .await;
    let helpers = f.sites(app, "run", "helper").await;
    assert_eq!(helpers.len(), 2);
    assert!(helpers.iter().all(|c| c.resolution == Resolution::Resolved));
    f.assert_external(app, "run", "getLogger", "org.slf4j.LoggerFactory")
        .await;
}

#[tokio::test]
async fn same_package_functions_and_types_are_visible_and_java_sees_kotlin() {
    let a = "package p\n\nfun top() {\n}\n\nclass B {\n    fun run() {\n    }\n}\n";
    let b = "package p\n\nclass C {\n    fun go() {\n        top()\n        val b = B()\n        b.run()\n    }\n}\n";
    let java = "package p;\n\npublic class J {\n  void go() { new B().run(); B b = new B(); b.run(); }\n}\n";
    let other = "package q\n\nfun go() {\n    top()\n}\n";
    let f = Fixture::new(&[
        ("p/A.kt", a),
        ("p/C.kt", b),
        ("p/J.java", java),
        ("q/D.kt", other),
    ])
    .await;
    f.assert_resolved("p/C.kt", "go", "top", "p/A.kt", "top")
        .await;
    f.assert_resolved("p/C.kt", "go", "run", "p/A.kt", "run")
        .await;
    let java_runs = f.sites("p/J.java", "go", "run").await;
    assert_eq!(
        java_runs[1].resolution,
        Resolution::Resolved,
        "Java resolves a Kotlin class"
    );
    f.assert_unresolved("q/D.kt", "go", "top", "unresolved-name")
        .await;
}
