//! Function and class metrics on fixture files whose numbers were worked out by hand.
use harness_code_analysis::metrics::{
    measure_file, metrics_supported, FileMetrics, FunctionKind, FunctionMetrics, MetricsError,
};
use harness_code_analysis::LanguageId;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/metrics/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn function<'a>(file: &'a FileMetrics, name: &str) -> &'a FunctionMetrics {
    file.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no {name} in {:?}",
                file.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
            )
        })
}

/// `(ccn, cognitive, max_nesting, bumps, params, nloc)`
fn shape(f: &FunctionMetrics) -> (u32, u32, u32, u32, u32, u32) {
    (f.ccn, f.cognitive, f.max_nesting, f.bumps, f.params, f.nloc)
}

#[test]
fn rust_functions() {
    let file = measure_file(&LanguageId::Rust, "sample.rs", &fixture("sample.rs")).unwrap();
    assert!(!file.partial);
    let names: Vec<&str> = file
        .functions
        .iter()
        .map(|f| f.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "add",
            "classify",
            "kind",
            "Counter.new",
            "Counter.bump",
            "Counter.total",
            "Counter.size",
            "Counter.name_of",
            "map callback"
        ]
    );
    assert_eq!(shape(function(&file, "add")), (1, 0, 0, 0, 2, 3));
    // if (+1) with && (+1), else if (+1) with || (+1), else, a loop and an if inside it.
    assert_eq!(shape(function(&file, "classify")), (7, 10, 3, 1, 2, 14));
    // Two non-wildcard arms; the match nests once.
    assert_eq!(shape(function(&file, "kind")), (3, 1, 1, 0, 1, 7));
    let bump = function(&file, "bump");
    assert_eq!(
        (bump.kind, bump.params, bump.class),
        (FunctionKind::Method, 0, Some(0))
    );
    let closure = function(&file, "map callback");
    assert_eq!(
        (closure.kind, closure.params, closure.class),
        (FunctionKind::Closure, 1, None)
    );
    assert_eq!(
        (
            function(&file, "classify").start_line,
            function(&file, "classify").end_line
        ),
        (9, 23)
    );
}

#[test]
fn rust_impl_cohesion() {
    let file = measure_file(&LanguageId::Rust, "sample.rs", &fixture("sample.rs")).unwrap();
    assert_eq!(file.classes.len(), 1);
    let counter = &file.classes[0];
    assert_eq!(counter.name, "Counter");
    assert_eq!(
        (
            counter.method_count,
            counter.field_count,
            counter.max_method_ccn
        ),
        (5, 2, 1)
    );
    // bump/total/size share `count` (total calls size); name_of alone uses `names`; new touches nothing.
    assert_eq!(counter.lcom4, 2);
    assert_eq!(counter.nloc, 18);
    assert_eq!(file.nloc, 47);
}

#[test]
fn rust_bumps_count_separate_nested_blocks() {
    let source = b"fn bumpy(a: bool, b: bool) {\n    if a { if b { x(); } }\n    for _ in 0..3 { while b { y(); } }\n    if b { z(); }\n}\n";
    let file = measure_file(&LanguageId::Rust, "b.rs", source).unwrap();
    let f = &file.functions[0];
    assert_eq!((f.bumps, f.max_nesting, f.ccn), (2, 2, 6));
}

#[test]
fn typescript_functions_and_class() {
    let file = measure_file(&LanguageId::TypeScript, "sample.ts", &fixture("sample.ts")).unwrap();
    assert!(!file.partial);
    let names: Vec<&str> = file
        .functions
        .iter()
        .map(|f| f.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "total",
            "Cart.add",
            "Cart.count",
            "Cart.rename",
            "Cart.describe",
            "Cart.empty",
            "describe callback",
            "it callback"
        ]
    );
    assert_eq!(shape(function(&file, "total")), (6, 6, 2, 1, 2, 11));
    // `this: Cart` declares the receiver, not an argument.
    assert_eq!(function(&file, "add").params, 1);
    assert_eq!(function(&file, "describe").kind, FunctionKind::Method);
    // Two cases, the default does not count.
    let it = function(&file, "it callback");
    assert_eq!(
        (it.ccn, it.cognitive, it.max_nesting, it.bumps),
        (3, 1, 1, 0)
    );
    let cart = &file.classes[0];
    assert_eq!(
        (
            cart.name.as_str(),
            cart.method_count,
            cart.field_count,
            cart.lcom4
        ),
        ("Cart", 5, 2, 1)
    );
}

#[test]
fn python_functions_and_class() {
    let file = measure_file(&LanguageId::Python, "sample.py", &fixture("sample.py")).unwrap();
    assert!(!file.partial);
    let names: Vec<&str> = file
        .functions
        .iter()
        .map(|f| f.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "score",
            "Account.__init__",
            "Account.deposit",
            "Account.report",
            "Account.parse",
            "key",
            "Account.audit"
        ]
    );
    // and/or are two runs; elif and else are flat; the comprehension `if` and the conditional
    // expression add one each. The docstring is not code.
    assert_eq!(shape(function(&file, "score")), (8, 9, 2, 1, 3, 11));
    assert_eq!(function(&file, "__init__").params, 1);
    let deposit = function(&file, "deposit");
    assert_eq!(
        (deposit.ccn, deposit.cognitive, deposit.max_nesting),
        (2, 1, 1)
    );
    // A static method keeps its first parameter.
    assert_eq!(function(&file, "parse").params, 1);
    assert_eq!(function(&file, "audit").ccn, 2);
    let account = &file.classes[0];
    assert_eq!(
        (
            account.method_count,
            account.field_count,
            account.lcom4,
            account.max_method_ccn
        ),
        (5, 2, 1, 2)
    );
}

/// `(method_count, field_count, lcom4, max_method_ccn)`
fn class_shape(file: &FileMetrics) -> (u32, u32, u32, u32) {
    let class = &file.classes[0];
    (
        class.method_count,
        class.field_count,
        class.lcom4,
        class.max_method_ccn,
    )
}

#[test]
fn kotlin_functions_and_class() {
    let file = measure_file(&LanguageId::Kotlin, "sample.kt", &fixture("sample.kt")).unwrap();
    assert!(!file.partial);
    let names: Vec<&str> = file
        .functions
        .iter()
        .map(|f| f.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "classify",
            "Account.deposit",
            "Account.report",
            "Account.describe",
            "Account.toString",
            "Account.constructor",
            "Account.parse",
            "total",
            "filter callback",
            "sumOf callback"
        ]
    );
    // Same shape as the Rust `classify`: `else if` and `else` are flat, the loop and the `if`
    // in it nest under the `else`.
    assert_eq!(shape(function(&file, "classify")), (7, 10, 3, 1, 2, 14));
    assert_eq!(function(&file, "classify").typed_params, 2);
    assert_eq!(function(&file, "classify").primitive_params, 2);
    // Two non-default `when` entries.
    let describe = function(&file, "describe");
    assert_eq!(
        (describe.ccn, describe.cognitive, describe.max_nesting),
        (3, 1, 1)
    );
    assert!(function(&file, "toString").inherited_signature);
    assert!(function(&file, "constructor").constructor);
    assert_eq!(function(&file, "constructor").params, 1);
    assert_eq!(function(&file, "filter callback").params, 0);
    assert_eq!(function(&file, "sumOf callback").params, 1);
    // owner, balance (constructor properties) and history are fields, read without `this.`;
    // a local `owner` in toString shadows the field. The companion's `parse` counts as a method.
    assert_eq!(class_shape(&file), (6, 3, 1, 3));
    assert_eq!(file.classes[0].name, "Account");
}

#[test]
fn java_functions_and_class() {
    let file = measure_file(&LanguageId::Java, "Sample.java", &fixture("Sample.java")).unwrap();
    assert!(!file.partial);
    let names: Vec<&str> = file
        .functions
        .iter()
        .map(|f| f.qualified_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Account.Account",
            "Account.deposit",
            "Account.describe",
            "Account.label",
            "Account.toString",
            "Account.parse",
            "Account.positives",
            "<lambda>"
        ]
    );
    let deposit = function(&file, "deposit");
    assert_eq!(
        (
            deposit.ccn,
            deposit.cognitive,
            deposit.max_nesting,
            deposit.bumps
        ),
        (6, 9, 3, 1)
    );
    // Three non-default case labels.
    let describe = function(&file, "describe");
    assert_eq!(
        (describe.ccn, describe.cognitive, describe.max_nesting),
        (4, 1, 1)
    );
    assert_eq!(
        (
            function(&file, "parse").ccn,
            function(&file, "parse").cognitive
        ),
        (2, 1)
    );
    assert!(function(&file, "Account").constructor);
    assert!(function(&file, "toString").inherited_signature);
    assert_eq!(function(&file, "<lambda>").params, 1);
    // balance, history and owner; `this.owner = owner` reads the parameter, not the field.
    assert_eq!(class_shape(&file), (7, 3, 1, 6));
}

#[test]
fn a_class_split_in_two_has_lcom4_of_two() {
    let source = b"class Split:\n    def a(self):\n        return self.x\n    def b(self):\n        self.x = 1\n    def c(self):\n        return self.y\n    def d(self):\n        return self.c()\n    def helper(self):\n        return 1\n";
    let file = measure_file(&LanguageId::Python, "s.py", source).unwrap();
    assert_eq!(file.classes[0].lcom4, 2);
    assert_eq!(file.classes[0].field_count, 2);
}

#[test]
fn javascript_and_tsx_parse_with_their_grammars() {
    let js = measure_file(
        &LanguageId::JavaScript,
        "a.js",
        b"function f(a, b = 1, ...rest) { return a || b; }\nconst g = x => x;\n",
    )
    .unwrap();
    assert_eq!(
        js.functions
            .iter()
            .map(|f| (f.name.as_str(), f.params, f.ccn))
            .collect::<Vec<_>>(),
        [("f", 3, 2), ("g", 1, 1)]
    );
    let tsx = measure_file(
        &LanguageId::TypeScript,
        "a.tsx",
        b"export const View = ({ n }: { n: number }) => <div>{n > 1 ? \"many\" : \"one\"}</div>;\n",
    )
    .unwrap();
    assert!(!tsx.partial);
    assert_eq!(
        (tsx.functions[0].name.as_str(), tsx.functions[0].ccn),
        ("View", 2)
    );
}

#[test]
fn broken_files_are_partial_and_other_languages_unsupported() {
    let broken = measure_file(
        &LanguageId::Rust,
        "x.rs",
        b"fn ok() { if a { 1 } }\nfn broken( {\n",
    )
    .unwrap();
    assert!(broken.partial);
    assert_eq!(broken.functions[0].name, "ok");
    assert!(matches!(
        measure_file(&LanguageId::Go, "x.go", b"package x"),
        Err(MetricsError::Unsupported(_))
    ));
    assert!(metrics_supported(&LanguageId::Python) && !metrics_supported(&LanguageId::Go));
}

#[test]
fn metrics_are_deterministic() {
    let a = measure_file(&LanguageId::TypeScript, "sample.ts", &fixture("sample.ts")).unwrap();
    let b = measure_file(&LanguageId::TypeScript, "sample.ts", &fixture("sample.ts")).unwrap();
    assert_eq!(a, b);
}

#[test]
fn every_source_file_of_this_crate_measures_without_errors() {
    let mut stack = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src"
    ))];
    let mut measured = 0;
    let mut partial = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let file = measure_file(
                    &LanguageId::Rust,
                    &path.to_string_lossy(),
                    &std::fs::read(&path).unwrap(),
                )
                .unwrap();
                // The grammar may not know the newest syntax; such a file is still measured, marked partial.
                partial += usize::from(file.partial);
                assert!(file
                    .functions
                    .iter()
                    .all(|f| f.ccn >= 1 && f.nloc >= 1 && f.start_line <= f.end_line));
                measured += 1;
            }
        }
    }
    assert!(measured > 20);
    assert!(
        partial * 10 < measured,
        "{partial} of {measured} files did not parse cleanly"
    );
}

#[test]
fn conditions_record_operator_count_mix_and_negation() {
    let source = b"fn f(a: bool, b: bool, c: bool, d: bool) -> bool {\n    if a && b {}\n    if a && (b || !c) && d { return true; }\n    a || b || c\n}\n";
    let file = measure_file(&LanguageId::Rust, "c.rs", source).unwrap();
    let conditions = &file.functions[0].conditions;
    assert_eq!(conditions.len(), 2, "a single operator is not recorded");
    assert_eq!(
        (
            conditions[0].line,
            conditions[0].operators,
            conditions[0].mixed,
            conditions[0].negated
        ),
        (3, 3, true, true)
    );
    assert_eq!(
        (
            conditions[1].operators,
            conditions[1].mixed,
            conditions[1].negated
        ),
        (2, false, false)
    );
    let py = measure_file(
        &LanguageId::Python,
        "c.py",
        b"def f(a, b, c):\n    return a and not b and c\n",
    )
    .unwrap();
    assert_eq!(
        (
            py.functions[0].conditions[0].operators,
            py.functions[0].conditions[0].negated
        ),
        (2, true)
    );
}

#[test]
fn typed_and_primitive_parameters_constructors_and_inherited_signatures() {
    let rust = measure_file(
        &LanguageId::Rust,
        "p.rs",
        b"struct S;\nimpl S {\n    fn new(a: u32, b: &str, c: String, d: Vec<u8>, e: &mut bool) -> Self { S }\n}\nimpl Clone for S {\n    fn clone(&self) -> Self { S }\n}\n",
    )
    .unwrap();
    let new = function(&rust, "new");
    assert_eq!(
        (
            new.params,
            new.typed_params,
            new.primitive_params,
            new.constructor,
            new.inherited_signature
        ),
        (5, 5, 4, true, false)
    );
    assert!(function(&rust, "clone").inherited_signature);

    let ts = measure_file(
        &LanguageId::TypeScript,
        "p.ts",
        b"class A { constructor(a: string, b: number, c: Date, d) {} override run(x: boolean) {} }\n",
    )
    .unwrap();
    let ctor = function(&ts, "constructor");
    assert_eq!(
        (
            ctor.params,
            ctor.typed_params,
            ctor.primitive_params,
            ctor.constructor
        ),
        (4, 3, 2, true)
    );
    assert!(function(&ts, "run").inherited_signature);

    let py = measure_file(
        &LanguageId::Python,
        "p.py",
        b"class A:\n    def __init__(self, a: int, b: str, c: list, d=1):\n        pass\n",
    )
    .unwrap();
    let init = function(&py, "__init__");
    assert_eq!(
        (
            init.params,
            init.typed_params,
            init.primitive_params,
            init.constructor
        ),
        (4, 3, 2, true)
    );
}
