mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn extracts_methods_constructors_blocks_lambdas_and_owners() {
    let src = "class Foo\n  CONST = make\n  def initialize(x); run; end\n  def run\n    go\n    go\n    l = lambda { |q| q }\n    l.call(1)\n    [1].each { |i| puts i }\n  end\n  def go; end\n  def self.build; new; end\nend\ndef make; end\nmake\n";
    let f = Fixture::new(&[("a.rb", src)]).await;
    assert_eq!(
        f.callable_by_qualified("a.rb", "Foo.initialize").kind,
        CallableKind::Constructor
    );
    assert_eq!(
        f.callable_by_qualified("a.rb", "Foo.run").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.rb", "Foo.build").kind,
        CallableKind::Method
    );
    assert_eq!(
        f.callable_by_qualified("a.rb", "make").kind,
        CallableKind::Function
    );
    let lambda = f.callable_by_qualified("a.rb", "Foo.run.l");
    assert_eq!(lambda.kind, CallableKind::Closure);
    assert_eq!(lambda.owner.as_ref(), Some(&f.callable("a.rb", "run").id));
    // Bare identifiers are implicit-self calls with distinct spans; known locals are not calls.
    let gos = f.sites("a.rb", "run", "go").await;
    assert_eq!(gos.len(), 2);
    assert_ne!(gos[0].span.start_byte, gos[1].span.start_byte);
    let go = &f.callable("a.rb", "go").id;
    assert!(gos
        .iter()
        .all(|c| c.resolution == Resolution::Resolved && c.target.as_ref() == Some(go)));
    let run_calls: Vec<_> = f
        .calls_in("a.rb", "run")
        .await
        .iter()
        .map(|c| f.callee_text(c).to_owned())
        .collect();
    assert!(
        !run_calls.iter().any(|t| t == "q" || t == "i"),
        "block params are locals: {run_calls:?}"
    );
    // Class-body and top-level calls have synthetic owners.
    let init = f.callable_by_qualified("a.rb", "Foo.<init:Foo>");
    assert_eq!(f.calls(init).await.len(), 1);
    let module = f.callable_by_qualified("a.rb", "<module>");
    assert_eq!(f.calls(module).await.len(), 1);
    f.assert_resolved("a.rb", "initialize", "run", "a.rb", "run")
        .await;
    // `l.call(1)` goes through the lambda value, not a method named `call`.
    f.assert_unresolved("a.rb", "run", "call", "dynamic-receiver")
        .await;
}

#[tokio::test]
async fn shadowing_requires_constructors_and_class_methods() {
    let src = "def target; end
def plain; target; end
def via_param(target); target; end
def via_local; target = 1; target; end
def via_block; [1].each { |target| target }; end
";
    let f = Fixture::new(&[("a.rb", src)]).await;
    f.assert_resolved("a.rb", "plain", "target", "a.rb", "target")
        .await;
    // Parameters and assigned locals are variables, so there is no call to `target` at all.
    assert!(f.sites("a.rb", "via_param", "target").await.is_empty());
    assert!(f.sites("a.rb", "via_local", "target").await.is_empty());
    assert!(f.sites("a.rb", "via_block", "target").await.is_empty());

    let lib = "class Widget
  def initialize(n); end
  def spin; end
  def self.build; end
  def self.helper; end
end
class Bare; end
def util; end
";
    let main = "require_relative 'lib/widget'
require 'json'
require_relative 'missing'
def go
  w = Widget.new(1)
  w.spin
  Widget.build
  util
  Bare.new
  unknown_call
  w.undefined_method
  JSON.parse('{}')
end
";
    let f = Fixture::new(&[("lib/widget.rb", lib), ("main.rb", main)]).await;
    f.assert_resolved("main.rb", "go", "Widget", "lib/widget.rb", "initialize")
        .await;
    f.assert_resolved("main.rb", "go", "spin", "lib/widget.rb", "spin")
        .await;
    f.assert_resolved("main.rb", "go", "build", "lib/widget.rb", "build")
        .await;
    f.assert_resolved("main.rb", "go", "util", "lib/widget.rb", "util")
        .await;
    f.assert_unresolved("main.rb", "go", "Bare", "implicit-constructor")
        .await;
    f.assert_unresolved("main.rb", "go", "unknown_call", "unresolved-name")
        .await;
    f.assert_unresolved("main.rb", "go", "undefined_method", "method-not-in-type")
        .await;
    f.assert_unresolved("main.rb", "go", "parse", "dynamic-receiver")
        .await;
    let evidence = f.site("main.rb", "go", "util").await.evidence.unwrap();
    assert_eq!(evidence.rule_id, "require");
    assert!(main[evidence.span.unwrap().start_byte as usize..]
        .starts_with("require_relative 'lib/widget'"));
}

#[tokio::test]
async fn files_that_are_not_required_are_not_visible() {
    let a = "def hidden; end
";
    let b = "def go; hidden; end
";
    let f = Fixture::new(&[("a.rb", a), ("b.rb", b)]).await;
    f.assert_unresolved("b.rb", "go", "hidden", "unresolved-name")
        .await;
}

#[tokio::test]
async fn reopened_classes_and_mixins_keep_uncertainty() {
    let base = "module Greeter
  def greet; end
end
class Person
  include Greeter
  def run; greet; end
  def speak; end
end
";
    let reopen = "require_relative 'base'
class Person
  def speak; end
end
def go
  p = Person.new
  p.speak
  p.greet
end
";
    let f = Fixture::new(&[("base.rb", base), ("reopen.rb", reopen)]).await;
    // `speak` is defined in both files: two possible targets, never one exact edge.
    let speak = f.site("reopen.rb", "go", "speak").await;
    assert_eq!(speak.resolution, Resolution::Possible);
    assert!(speak.target.is_none());
    let mut files: Vec<_> = speak
        .candidates
        .iter()
        .map(|id| {
            f.callables
                .iter()
                .find(|c| &c.id == id)
                .unwrap()
                .span
                .path
                .clone()
        })
        .collect();
    files.sort();
    assert_eq!(files, ["base.rb", "reopen.rb"]);
    // `greet` comes from the mixed-in module.
    f.assert_resolved_q("reopen.rb", "go", "greet", "base.rb", "Greeter.greet")
        .await;
    // Inside the class, the same mixin method is reached by an implicit-self call.
    f.assert_resolved_q("base.rb", "run", "greet", "base.rb", "Greeter.greet")
        .await;
}
