mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

const UTIL: &str = "namespace Ex.Util {\n  public static class Util {\n    public static void Stat() {}\n    public static void Other() {}\n  }\n  public class Helper {\n    public Helper() {}\n    public void Go() {}\n    public void Go(int a) {}\n  }\n}\n";

#[tokio::test]
async fn extracts_namespaces_constructors_accessors_local_functions_and_lambdas() {
    let src = "namespace Ex.App {\n  public class A {\n    int field = Make();\n    public A(int x) { Run(); Run(); }\n    public int Prop { get { return Make(); } }\n    void Run() {\n      void Local() { Run(); }\n      Func<int,int> f = q => Make();\n      Local();\n      f(1);\n    }\n    static int Make() { return 0; }\n  }\n}\n";
    let f = Fixture::new(&[("A.cs", src)]).await;
    assert_eq!(
        f.callable_by_qualified("A.cs", "Ex.App.A.A").kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable_by_qualified("A.cs", "Ex.App.A.get_Prop").kind,
        CallableKind::Method
    );
    let local = f.callable_by_qualified("A.cs", "Ex.App.A.Run.Local");
    assert_eq!(local.owner.as_ref(), Some(&f.callable("A.cs", "Run").id));
    let lambda = f.callable_by_qualified("A.cs", "Ex.App.A.Run.f");
    assert_eq!(lambda.kind, CallableKind::Closure);
    let init = f.callable_by_qualified("A.cs", "Ex.App.A.<init:A>");
    assert_eq!(f.calls(init).await.len(), 1);
    let runs = f.sites("A.cs", "A", "Run").await;
    assert_eq!(runs.len(), 2);
    assert_ne!(runs[0].span.start_byte, runs[1].span.start_byte);
    f.assert_resolved("A.cs", "Local", "Run", "A.cs", "Run")
        .await;
    f.assert_resolved("A.cs", "get_Prop", "Make", "A.cs", "Make")
        .await;
    // The local function is bound lexically and the lambda by its variable.
    f.assert_resolved_q("A.cs", "Run", "Local", "A.cs", "Ex.App.A.Run.Local")
        .await;
    f.assert_resolved_q("A.cs", "Run", "f", "A.cs", "Ex.App.A.Run.f")
        .await;
}

#[tokio::test]
async fn shadowing_by_parameters_and_locals() {
    let src = "class A {\n  void Target() {}\n  void Plain() { Target(); }\n  void ViaParam(Action Target) { Target(); }\n  void ViaLocal() { Action Target = () => {}; Target(); }\n}\n";
    let f = Fixture::new(&[("A.cs", src)]).await;
    f.assert_resolved_q("A.cs", "Plain", "Target", "A.cs", "A.Target")
        .await;
    f.assert_unresolved("A.cs", "ViaParam", "Target", "local-value-callee")
        .await;
    f.assert_resolved_q("A.cs", "ViaLocal", "Target", "A.cs", "A.ViaLocal.Target")
        .await;
}

#[tokio::test]
async fn cross_file_usings_aliases_static_usings_and_overloads() {
    let main = "using Ex.Util;\nusing static Ex.Util.Util;\nusing Al = Ex.Util;\nusing System;\nnamespace Ex.App {\n  class Main {\n    void Go() {\n      var h = new Helper();\n      h.Go();\n      Stat();\n      Util.Other();\n      Al.Util.Other();\n      Console.WriteLine();\n      new Missing();\n    }\n  }\n}\n";
    let f = Fixture::new(&[("Util.cs", UTIL), ("Main.cs", main)]).await;
    f.assert_resolved("Main.cs", "Go", "Helper", "Util.cs", "Helper")
        .await;
    f.assert_possible_in("Main.cs", "Go", "Go", 2, "Util.cs")
        .await;
    f.assert_resolved("Main.cs", "Go", "Stat", "Util.cs", "Stat")
        .await;
    let others = f.sites("Main.cs", "Go", "Other").await;
    assert_eq!(others.len(), 2);
    let other = &f.callable("Util.cs", "Other").id;
    assert!(others
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(other)));
    f.assert_unresolved("Main.cs", "Go", "WriteLine", "dynamic-receiver")
        .await;
    f.assert_unresolved("Main.cs", "Go", "Missing", "unresolved-type")
        .await;
    let evidence = f.site("Main.cs", "Go", "Stat").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "using-static");
}

#[tokio::test]
async fn namespaces_scope_type_visibility_and_inheritance() {
    let base = "namespace Ex.Two;\npublic class Base { public void Hello() {} public virtual void Work() {} }\n";
    let derived = "namespace Ex.Two;\npublic class Derived : Base {\n  public override void Work() { base.Work(); Hello(); this.Hello(); }\n}\n";
    let sibling = "namespace Ex.Two;\nclass Sibling { void Go() { new Derived().Work(); Derived d = new Derived(); d.Work(); } }\n";
    let other = "namespace Ex.Three;\nclass Elsewhere { void Go() { new Derived(); } }\n";
    let files = [
        ("Base.cs", base),
        ("Derived.cs", derived),
        ("Sibling.cs", sibling),
        ("Other.cs", other),
    ];
    let f = Fixture::new(&files).await;
    f.assert_resolved_q("Derived.cs", "Work", "Work", "Base.cs", "Ex.Two.Base.Work")
        .await;
    let hellos = f.sites("Derived.cs", "Work", "Hello").await;
    assert_eq!(hellos.len(), 2);
    let hello = &f.callable_by_qualified("Base.cs", "Ex.Two.Base.Hello").id;
    assert!(
        hellos
            .iter()
            .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(hello)),
        "inherited methods resolve through the workspace base type: {hellos:?}"
    );
    // The sibling namespace member is visible without a using; the other namespace is not.
    let sibling_calls = f.sites("Sibling.cs", "Go", "Derived").await;
    assert!(sibling_calls
        .iter()
        .all(|c| c.evidence.as_ref().unwrap().rule_id == "implicit-constructor"));
    f.assert_unresolved("Other.cs", "Go", "Derived", "unresolved-type")
        .await;
    // d is exactly a Derived, so Work resolves to the override.
    let work = f.sites("Sibling.cs", "Go", "Work").await;
    assert_eq!(work[1].resolution, Resolution::Resolved);
    assert_eq!(
        work[1].target.as_ref(),
        Some(
            &f.callable_by_qualified("Derived.cs", "Ex.Two.Derived.Work")
                .id
        )
    );
}
