mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn extracts_ownership_constructors_closures_and_module_owner() {
    let src = "class Foo(Base):\n    registry = make()\n    def __init__(self, x):\n        self.run(x)\n    def run(self, x):\n        helper = lambda q: q\n        helper(1)\n        helper(2)\n    @staticmethod\n    def s():\n        pass\n\ndef outer():\n    def inner():\n        pass\n    inner()\n\nouter()\n";
    let f = Fixture::new(&[("m.py", src)]).await;
    assert_eq!(
        f.callable_by_qualified("m.py", "Foo.__init__").kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable_by_qualified("m.py", "Foo.run").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("m.py", "Foo.s").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("m.py", "outer").kind,
        CallableKind::Function
    );
    let inner = f.callable_by_qualified("m.py", "outer.inner");
    assert_eq!(inner.owner.as_ref(), Some(&f.callable("m.py", "outer").id));
    let lambda = f.callable_by_qualified("m.py", "Foo.run.helper");
    assert_eq!(lambda.kind, CallableKind::Closure);
    assert_eq!(lambda.owner.as_ref(), Some(&f.callable("m.py", "run").id));
    // `name` spans are exact bytes of the identifier.
    let run = f.callable("m.py", "run");
    assert_eq!(
        &src[run.span.start_byte as usize..run.span.end_byte as usize],
        "run"
    );
    // Repeated calls keep distinct spans.
    let helper_calls = f.sites("m.py", "run", "helper").await;
    assert_eq!(helper_calls.len(), 2);
    assert!(helper_calls[0].span.start_byte < helper_calls[1].span.start_byte);
    // Class-body and module-level calls have synthetic owners, not a nearby method.
    let class_init = f.callable_by_qualified("m.py", "Foo.<init:Foo>");
    assert_eq!(class_init.kind, CallableKind::Synthetic);
    assert_eq!(f.calls(class_init).await.len(), 1);
    let module = f.callable_by_qualified("m.py", "<module>");
    assert_eq!(f.calls(module).await.len(), 1);
}

#[tokio::test]
async fn shadowing_and_same_file_resolution() {
    let src = "def target():\n    pass\n\ndef plain():\n    target()\n\ndef via_param(target):\n    target()\n\ndef via_assign():\n    target = lambda: 0\n    target()\n\ndef via_nested():\n    def target():\n        pass\n    target()\n\ndef recursive():\n    recursive()\n";
    let f = Fixture::new(&[("a.py", src)]).await;
    f.assert_resolved_q("a.py", "plain", "target", "a.py", "target")
        .await;
    f.assert_unresolved("a.py", "via_param", "target", "local-value-callee")
        .await;
    f.assert_resolved_q("a.py", "via_assign", "target", "a.py", "via_assign.target")
        .await;
    f.assert_resolved_q("a.py", "via_nested", "target", "a.py", "via_nested.target")
        .await;
    f.assert_resolved("a.py", "recursive", "recursive", "a.py", "recursive")
        .await;
}

#[tokio::test]
async fn cross_file_imports_aliases_relative_and_packages() {
    let files = [
        ("helper.py", "def helper():\n    pass\n\ndef other():\n    pass\n"),
        ("pkg/__init__.py", "from .impl import exported\n"),
        ("pkg/impl.py", "def exported():\n    pass\n"),
        ("pkg/sib.py", "def sibling():\n    pass\n"),
        ("pkg/user.py", "from . import sib\nfrom .impl import exported as renamed\nfrom ..helper import helper\ndef go():\n    sib.sibling()\n    renamed()\n    helper()\n"),
        ("main.py", "from helper import helper, other as o\nimport helper as h\nimport pkg.sib\nimport os\nfrom pkg import exported\nfrom nowhere import ghost\ndef go():\n    helper()\n    o()\n    h.other()\n    pkg.sib.sibling()\n    exported()\n    os.getcwd()\n    ghost()\n    unknown()\n"),
    ];
    let f = Fixture::new(&files).await;
    f.assert_resolved("main.py", "go", "helper", "helper.py", "helper")
        .await;
    f.assert_resolved("main.py", "go", "o", "helper.py", "other")
        .await;
    f.assert_resolved("main.py", "go", "other", "helper.py", "other")
        .await;
    f.assert_resolved("main.py", "go", "sibling", "pkg/sib.py", "sibling")
        .await;
    // Re-export through the package __init__ is followed to the defining module.
    f.assert_resolved("main.py", "go", "exported", "pkg/impl.py", "exported")
        .await;
    f.assert_external("main.py", "go", "getcwd", "os").await;
    f.assert_external("main.py", "go", "ghost", "nowhere").await;
    f.assert_unresolved("main.py", "go", "unknown", "unresolved-name")
        .await;
    // Relative imports.
    f.assert_resolved("pkg/user.py", "go", "sibling", "pkg/sib.py", "sibling")
        .await;
    f.assert_resolved("pkg/user.py", "go", "renamed", "pkg/impl.py", "exported")
        .await;
    f.assert_resolved("pkg/user.py", "go", "helper", "helper.py", "helper")
        .await;
    let evidence = f.site("main.py", "go", "helper").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "static-import");
    assert_eq!(evidence.span.unwrap().path, "main.py");
}

#[tokio::test]
async fn unimported_cross_file_names_stay_unresolved() {
    let files = [
        ("helper.py", "def helper():\n    pass\n"),
        ("main.py", "def go():\n    helper()\n"),
    ];
    let f = Fixture::new(&files).await;
    f.assert_unresolved("main.py", "go", "helper", "unresolved-name")
        .await;
}

#[tokio::test]
async fn classes_constructors_self_and_typed_receivers() {
    let files = [
        ("models.py", "class Base:\n    def base_method(self):\n        pass\n\nclass Foo(Base):\n    def __init__(self, x):\n        self.setup()\n    def setup(self):\n        self.base_method()\n    def go(self):\n        pass\n    @classmethod\n    def build(cls):\n        cls.make()\n    @classmethod\n    def make(cls):\n        pass\n\nclass Bare:\n    pass\n"),
        ("main.py", "from models import Foo, Bare\ndef run():\n    f = Foo(1)\n    f.go()\n    f.base_method()\n    f.missing()\n    Bare()\n    Foo.make()\n    dyn = getattr(f, 'go')\n    dyn()\n    f.items[0]()\n"),
    ];
    let f = Fixture::new(&files).await;
    f.assert_resolved_q("main.py", "run", "Foo", "models.py", "Foo.__init__")
        .await;
    f.assert_resolved_q("main.py", "run", "go", "models.py", "Foo.go")
        .await;
    // Inherited method found through the workspace base class.
    f.assert_resolved_q(
        "main.py",
        "run",
        "base_method",
        "models.py",
        "Base.base_method",
    )
    .await;
    f.assert_unresolved("main.py", "run", "missing", "method-not-in-type")
        .await;
    f.assert_unresolved("main.py", "run", "Bare", "implicit-constructor")
        .await;
    f.assert_resolved_q("main.py", "run", "make", "models.py", "Foo.make")
        .await;
    // self / cls calls inside the class.
    f.assert_resolved_q("models.py", "__init__", "setup", "models.py", "Foo.setup")
        .await;
    f.assert_resolved_q(
        "models.py",
        "setup",
        "base_method",
        "models.py",
        "Base.base_method",
    )
    .await;
    f.assert_resolved_q("models.py", "build", "make", "models.py", "Foo.make")
        .await;
    // Dynamic negatives.
    f.assert_unresolved("main.py", "run", "dyn", "local-value-callee")
        .await;
    f.assert_unresolved("main.py", "run", "getattr", "unresolved-name")
        .await;
    let dynamic = f.calls_in("main.py", "run").await;
    assert!(dynamic
        .iter()
        .any(|c| f.callee_text(c).starts_with("f.items[0]")
            && c.resolution == Resolution::Unresolved));
}

#[tokio::test]
async fn overloaded_redefinitions_are_possible_not_exact() {
    let src = "def pick():\n    pass\n\ndef pick():\n    pass\n\ndef go():\n    pick()\n";
    let f = Fixture::new(&[("a.py", src)]).await;
    let pick: Vec<_> = f.callables.iter().filter(|c| c.name == "pick").collect();
    assert_eq!(pick.len(), 2);
    let site = f.site("a.py", "go", "pick").await;
    assert_eq!(site.resolution, Resolution::Possible);
    assert_eq!(site.candidates.len(), 2);
    assert!(site.target.is_none());
}

#[tokio::test]
async fn malformed_syntax_is_partial_and_never_falsely_exact() {
    let src =
        "def callee():\n    pass\n\ndef broken(:\n    callee()\n\ndef fine():\n    callee()\n";
    let f = Fixture::new(&[("a.py", src)]).await;
    assert!(f.refresh.metadata.partial);
    assert!(f
        .refresh
        .metadata
        .diagnostics
        .iter()
        .any(|d| d.message.contains("syntax error")));
    // The intact function keeps its exact edge; nothing inside the error region claims one.
    f.assert_resolved("a.py", "fine", "callee", "a.py", "callee")
        .await;
    for caller in f.callables.iter().filter(|c| c.name == "broken") {
        for call in f.calls(caller).await {
            assert_ne!(call.resolution, Resolution::Resolved);
        }
    }
}
