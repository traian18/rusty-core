#![allow(dead_code)]
use harness_code_analysis::{
    metrics::{ClassMetrics, Condition, FileMetrics, FunctionKind, FunctionMetrics},
    LanguageId,
};
use harness_code_health::{detect_file, DetectorConfig, FileContext, Finding, Severity};

pub fn function(name: &str) -> FunctionMetrics {
    FunctionMetrics {
        name: name.into(),
        qualified_name: name.into(),
        kind: FunctionKind::Function,
        start_line: 10,
        end_line: 20,
        nloc: 10,
        ccn: 1,
        cognitive: 0,
        max_nesting: 0,
        bumps: 0,
        params: 0,
        typed_params: 0,
        primitive_params: 0,
        constructor: false,
        inherited_signature: false,
        conditions: Vec::new(),
        class: None,
        ..Default::default()
    }
}

pub fn class(name: &str, nloc: u32, methods: Vec<usize>, lcom4: u32) -> ClassMetrics {
    ClassMetrics {
        name: name.into(),
        start_line: 1,
        end_line: nloc + 1,
        nloc,
        method_count: methods.len() as u32,
        max_method_ccn: 1,
        field_count: 2,
        lcom4,
        methods,
    }
}

pub fn condition(operators: u32, mixed: bool, negated: bool) -> Condition {
    Condition {
        line: 12,
        operators,
        mixed,
        negated,
    }
}

pub fn file(functions: Vec<FunctionMetrics>, classes: Vec<ClassMetrics>, nloc: u32) -> FileMetrics {
    FileMetrics {
        language: LanguageId::Rust,
        nloc,
        partial: false,
        functions,
        classes,
        skipped_tests: 0,
    }
}

/// Findings of one detector, with the given importers.
pub fn run(
    detector: &str,
    metrics: &FileMetrics,
    importers: Option<u32>,
    p80: Option<u32>,
) -> Vec<Finding> {
    let mut context = FileContext::new("src/a.rs", metrics);
    context.importers = importers;
    context.repo_importers_p80 = p80;
    detect_file(&context, &DetectorConfig::default())
        .into_iter()
        .filter(|f| f.detector == detector)
        .collect()
}

/// The severity of a single function with `change` applied, or `None` when nothing fires.
pub fn severity_of(detector: &str, change: impl Fn(&mut FunctionMetrics)) -> Option<Severity> {
    let mut f = function("f");
    change(&mut f);
    let metrics = file(vec![f], Vec::new(), 500);
    let found = run(detector, &metrics, Some(100), None);
    assert!(found.len() <= 1, "{found:?}");
    found.first().map(|f| f.severity)
}
