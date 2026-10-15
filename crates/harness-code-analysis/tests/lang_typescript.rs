mod support;
use support::Fixture;

use harness_code_analysis::{CallableKind, Resolution};

#[tokio::test]
async fn type_only_imports_are_not_runtime_calls_and_annotations_type_receivers() {
    let lib = "export class Widget {\n  render(): void {}\n  static make(): Widget { return new Widget(); }\n}\nexport interface Shape { area(): number }\nexport function helper(): void {}\n";
    let main = "import type { helper } from './lib';\nimport { type Shape, Widget } from './lib';\nfunction go(w: Widget, s: Shape, x: Widget | null) {\n  helper();\n  w.render();\n  s.area();\n  x.render();\n  Widget.make();\n}\n";
    let f = Fixture::new(&[("lib.ts", lib), ("main.ts", main)]).await;
    // `helper` was imported with `import type`, so the call cannot be the exported function.
    f.assert_unresolved("main.ts", "go", "helper", "unresolved-name")
        .await;
    // Parameters annotated with a workspace class resolve; interface and union types do not.
    let renders = f.sites("main.ts", "go", "render").await;
    assert_eq!(renders.len(), 2);
    assert_eq!(renders[0].resolution, Resolution::Resolved, "w: Widget");
    assert_eq!(
        renders[1].resolution,
        Resolution::Unresolved,
        "union-typed receiver stays uncertain"
    );
    f.assert_unresolved("main.ts", "go", "area", "dynamic-receiver")
        .await;
    f.assert_resolved_q("main.ts", "go", "make", "lib.ts", "Widget.make")
        .await;
}

#[tokio::test]
async fn tsx_and_jsx_grammars_extract_components_and_calls() {
    let tsx = "import { util } from './util';
export function App(props: { n: number }) {
  const label = util();
  return <div onClick={() => util()}>{label}</div>;
}
";
    let util = "export function util() { return 1; }
";
    let jsx = "import { util } from './util.js';
export const Comp = () => <span>{util()}</span>;
";
    let f = Fixture::new(&[
        ("App.tsx", tsx),
        ("util.ts", util),
        ("Comp.jsx", jsx),
        ("util.js", util),
    ])
    .await;
    let utils = f.sites("App.tsx", "App", "util").await;
    assert_eq!(
        utils.len(),
        1,
        "the call inside the JSX handler belongs to the arrow closure"
    );
    assert_eq!(utils[0].resolution, Resolution::Resolved);
    let handler = f
        .callables
        .iter()
        .find(|c| c.span.path == "App.tsx" && c.name.starts_with("<closure@"))
        .expect("anonymous handler closure");
    assert_eq!(handler.kind, CallableKind::Closure);
    assert_eq!(
        handler.owner.as_ref(),
        Some(&f.callable("App.tsx", "App").id)
    );
    f.assert_resolved("Comp.jsx", "Comp", "util", "util.js", "util")
        .await;
}

#[tokio::test]
async fn commonjs_require_and_class_fields_and_dynamic_negatives() {
    let lib = "function helper() {}
function other() {}
module.exports = { helper, other };
";
    let main = "const { helper } = require('./lib');
const lib = require('./lib');
class K {
  handler = () => helper();
  go() { this.handler(); }
}
function dyn(obj, name) {
  obj[name]();
  obj.method();
  (0, obj.fn)();
  new obj.Ctor();
  import('./lib').then(() => {});
}
";
    let f = Fixture::new(&[("lib.js", lib), ("main.js", main)]).await;
    // CommonJS destructuring and namespace require bind to the module file.
    let sites = f.sites("main.js", "handler", "helper").await;
    assert_eq!(sites.len(), 1);
    let handler = f.callable_by_qualified("main.js", "K.handler");
    assert_eq!(
        handler.kind,
        CallableKind::Method,
        "class-field arrow functions are methods"
    );
    f.assert_resolved_q("main.js", "go", "handler", "main.js", "K.handler")
        .await;
    let calls = f.calls_in("main.js", "dyn").await;
    assert!(calls.len() >= 4);
    assert!(calls
        .iter()
        .all(|c| c.resolution == Resolution::Unresolved && c.target.is_none()));
    assert!(calls
        .iter()
        .any(|c| c.evidence.as_ref().unwrap().rule_id == "dynamic-callee"));
}
