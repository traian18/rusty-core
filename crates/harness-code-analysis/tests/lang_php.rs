mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn extracts_namespaces_classes_closures_and_module_owner() {
    let src = "<?php\nnamespace App\\Svc;\nclass A extends B {\n  public $f = 1;\n  public function __construct() { $this->run(); $this->run(); }\n  public function run() { $g = function() { helper(); }; $g(); $h = fn($x) => helper(); }\n  public static function make() {}\n}\nfunction helper() {}\nhelper();\n";
    let f = Fixture::new(&[("a.php", src)]).await;
    assert_eq!(
        f.callable_by_qualified("a.php", "App\\Svc.A.__construct")
            .kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable_by_qualified("a.php", "App\\Svc.A.run").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.php", "App\\Svc.helper").kind,
        CallableKind::Function
    );
    let closure = f.callable_by_qualified("a.php", "App\\Svc.A.run.$g");
    assert_eq!(closure.kind, CallableKind::Closure);
    assert_eq!(closure.owner.as_ref(), Some(&f.callable("a.php", "run").id));
    let runs = f.sites("a.php", "__construct", "run").await;
    assert_eq!(runs.len(), 2);
    assert_ne!(runs[0].span.start_byte, runs[1].span.start_byte);
    assert!(runs.iter().all(|c| c.resolution == Resolution::Resolved));
    let module = f.callable_by_qualified("a.php", "App\\Svc.<module>");
    assert_eq!(f.calls(module).await.len(), 1);
    // `$g()` is bound lexically to the closure; the closure's own `helper()` resolves in-namespace.
    f.assert_resolved_q("a.php", "run", "$g", "a.php", "App\\Svc.A.run.$g")
        .await;
}

#[tokio::test]
async fn variable_callees_do_not_collide_with_functions_and_dynamic_calls_stay_unresolved() {
    let src = "<?php\nfunction target() {}\nfunction plain() { target(); }\nfunction viaParam($target) { $target(); target(); }\nfunction dyn($name, $obj) { $name(); $obj->$name(); call_user_func('target'); }\n";
    let f = Fixture::new(&[("a.php", src)]).await;
    f.assert_resolved("a.php", "plain", "target", "a.php", "target")
        .await;
    // The parameter `$target` shadows nothing: the function call `target()` still resolves.
    f.assert_resolved("a.php", "viaParam", "target", "a.php", "target")
        .await;
    f.assert_unresolved("a.php", "viaParam", "$target", "local-value-callee")
        .await;
    let names = f.sites("a.php", "dyn", "$name").await;
    assert_eq!(names.len(), 2);
    let rules: Vec<_> = names
        .iter()
        .map(|c| c.evidence.as_ref().unwrap().rule_id.as_str())
        .collect();
    assert!(
        rules.contains(&"local-value-callee"),
        "variable function: {rules:?}"
    );
    assert!(
        rules.contains(&"dynamic-callee"),
        "variable method name: {rules:?}"
    );
    assert!(names.iter().all(|c| c.resolution == Resolution::Unresolved));
    f.assert_unresolved("a.php", "dyn", "call_user_func", "unresolved-name")
        .await;
}

#[tokio::test]
async fn cross_file_use_alias_function_import_and_namespaces() {
    let helper = "<?php\nnamespace Lib\\Util;\nclass Helper {\n  public function __construct() {}\n  public function go() {}\n  public static function stat() {}\n}\nfunction fn1() {}\n";
    let other = "<?php\nnamespace Lib;\nclass Other { public function __construct() {} }\n";
    let main = "<?php\nnamespace App;\nuse Lib\\Util\\Helper;\nuse function Lib\\Util\\fn1;\nuse Lib\\Other as O;\nuse Vendor\\Pkg\\Thing;\nfunction go() {\n  $h = new Helper();\n  $h->go();\n  Helper::stat();\n  fn1();\n  new O();\n  \\Lib\\Util\\fn1();\n  new Thing();\n  new Missing();\n  $x->unknown();\n}\n";
    let files = [
        ("lib/Helper.php", helper),
        ("lib/Other.php", other),
        ("app/main.php", main),
    ];
    let f = Fixture::new(&files).await;
    let m = "app/main.php";
    f.assert_resolved(m, "go", "Helper", "lib/Helper.php", "__construct")
        .await;
    f.assert_resolved(m, "go", "go", "lib/Helper.php", "go")
        .await;
    f.assert_resolved(m, "go", "stat", "lib/Helper.php", "stat")
        .await;
    let fn1 = f.sites(m, "go", "fn1").await;
    assert_eq!(fn1.len(), 2, "imported and fully-qualified call");
    let target = &f.callable("lib/Helper.php", "fn1").id;
    assert!(fn1
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(target)));
    f.assert_resolved(m, "go", "O", "lib/Other.php", "__construct")
        .await;
    f.assert_external(m, "go", "Thing", "Vendor\\Pkg\\Thing")
        .await;
    f.assert_unresolved(m, "go", "Missing", "unresolved-type")
        .await;
    f.assert_unresolved(m, "go", "unknown", "dynamic-receiver")
        .await;
    let evidence = f.site(m, "go", "stat").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "static-import");
    assert!(main[evidence.span.unwrap().start_byte as usize..].starts_with("use Lib"));
}

#[tokio::test]
async fn same_namespace_types_inheritance_and_includes() {
    let base = "<?php\nnamespace P;\nclass Base { public function work() {} public function hello() {} }\n";
    let derived = "<?php\nnamespace P;\nclass Derived extends Base {\n  public function work() { parent::work(); $this->hello(); self::make(); }\n  public static function make() {}\n}\n";
    let user = "<?php\nnamespace P;\nclass User { function go(Base $b) { $b->work(); $d = new Derived(); $d->work(); } }\n";
    let elsewhere = "<?php\nnamespace Q;\nclass E { function go() { new Derived(); } }\n";
    let funcs = "<?php\nfunction included_fn() {}\n";
    let includer =
        "<?php\nrequire_once 'funcs.php';\nfunction go() { included_fn(); not_included(); }\n";
    let files = [
        ("Base.php", base),
        ("Derived.php", derived),
        ("User.php", user),
        ("E.php", elsewhere),
        ("funcs.php", funcs),
        ("includer.php", includer),
    ];
    let f = Fixture::new(&files).await;
    f.assert_resolved_q("Derived.php", "work", "work", "Base.php", "P.Base.work")
        .await;
    f.assert_resolved_q("Derived.php", "work", "hello", "Base.php", "P.Base.hello")
        .await;
    f.assert_resolved("Derived.php", "work", "make", "Derived.php", "make")
        .await;
    let works = f.sites("User.php", "go", "work").await;
    assert_eq!(
        works[0].resolution,
        Resolution::Possible,
        "declared Base may be a Derived"
    );
    assert_eq!(works[0].candidates.len(), 2);
    assert_eq!(
        works[1].resolution,
        Resolution::Resolved,
        "constructed Derived is exact"
    );
    f.assert_unresolved("E.php", "go", "Derived", "unresolved-type")
        .await;
    f.assert_resolved(
        "includer.php",
        "go",
        "included_fn",
        "funcs.php",
        "included_fn",
    )
    .await;
    f.assert_unresolved("includer.php", "go", "not_included", "unresolved-name")
        .await;
}

#[tokio::test]
async fn malformed_php_is_partial_and_not_exact() {
    let src = "<?php\nfunction callee() {}\nfunction broken() { callee( }\nfunction fine() { callee(); }\n";
    let f = Fixture::new(&[("a.php", src)]).await;
    assert!(f.refresh.metadata.partial);
    f.assert_resolved("a.php", "fine", "callee", "a.php", "callee")
        .await;
    for caller in f.callables.iter().filter(|c| c.name == "broken") {
        for call in f.calls(caller).await {
            assert_ne!(call.resolution, Resolution::Resolved);
        }
    }
}
