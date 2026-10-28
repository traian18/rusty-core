use harness_code_analysis::{metrics::measure_file, LanguageId};
use harness_guidelines::{
    check_file, guidelines_json_schema, retain_introduced, ChangedLines, FileInput, Guidelines,
    Import, Kinds, Severity,
};

const RULES: &str = r#"{
  "version": 1,
  "limits": { "max_file_lines": 12, "max_params": 3 },
  "detectors": { "large_method": { "threshold": 40 } },
  "rules": [
    { "id": "no-unwrap", "kind": "pattern", "lang": "rust",
      "query": "(call_expression function: (field_expression field: (field_identifier) @m (#eq? @m \"unwrap\"))) @violation",
      "exclude": ["**/tests/**"], "message": "Return an error instead of unwrap()" },
    { "id": "no-todo", "kind": "pattern", "regex": "TODO", "severity": "warn" },
    { "id": "ui-not-core", "kind": "boundary", "applies": ["src/ui/**"], "must_not_import": ["src/core/**"] },
    { "id": "small-ui", "kind": "metric", "applies": ["src/ui/**"], "max_file_lines": 4, "severity": "warn" },
    { "id": "words", "kind": "prose", "text": "Name things after what they do." }
  ]
}"#;

const SOURCE: &str = "fn load(a: u8, b: u8, c: u8, d: u8) -> u8 {\n    let x = read().unwrap();\n    // TODO tidy\n    x\n}\n";

fn guidelines() -> Guidelines {
    Guidelines::parse(
        Some(RULES),
        Some("General.\n## `src/ui/**`\nOne component per file.\n"),
    )
    .expect("valid rules")
}

fn check(
    path: &str,
    source: &str,
    imports: Option<&[Import]>,
) -> Vec<harness_guidelines::Violation> {
    let metrics = measure_file(&LanguageId::Rust, path, source.as_bytes()).ok();
    let input = FileInput {
        path,
        source,
        metrics: metrics.as_ref(),
        imports,
    };
    check_file(&guidelines(), &input, Kinds::ALL)
}

#[test]
fn every_kind_of_rule_reports_where_it_applies() {
    let found = check("src/core/load.rs", SOURCE, None);
    let rules: Vec<&str> = found.iter().map(|v| v.rule.as_str()).collect();
    assert_eq!(rules, ["limits", "no-unwrap", "no-todo"], "{found:#?}");
    assert_eq!(found[0].function.as_deref(), Some("load"));
    assert!(found[0].message.contains("4 parameters"));
    assert_eq!(
        (found[1].start_line, found[1].severity),
        (2, Severity::Error)
    );
    assert_eq!(found[2].severity, Severity::Warn);
    assert!(check("src/core/tests/load.rs", SOURCE, None)
        .iter()
        .all(|v| v.rule != "no-unwrap"));
}

#[test]
fn boundaries_and_narrower_limits_apply_to_their_files_only() {
    let imports = [
        Import {
            target: "src/core/db.rs".into(),
            line: 1,
        },
        Import {
            target: "src/ui/theme.rs".into(),
            line: 2,
        },
    ];
    let found = check("src/ui/view.rs", SOURCE, Some(&imports));
    let boundary: Vec<_> = found.iter().filter(|v| v.rule == "ui-not-core").collect();
    assert_eq!(boundary.len(), 1);
    assert_eq!(boundary[0].start_line, 1);
    // The ui rule narrows the file limit to 4 lines, as a warning.
    let file_wide = found.iter().find(|v| v.file_wide()).expect("over 4 lines");
    assert_eq!(
        (file_wide.measured, file_wide.severity),
        (Some(5), Severity::Warn)
    );
}

#[test]
fn only_what_the_change_introduced_counts() {
    let found = check("src/core/load.rs", SOURCE, None);
    // Line 3 changed: the TODO, and the function spanning it.
    let edit = ChangedLines {
        ranges: vec![(3, 3)],
        base_line_count: Some(5),
    };
    let kept: Vec<_> = retain_introduced(found.clone(), &edit)
        .into_iter()
        .map(|v| v.rule)
        .collect();
    assert_eq!(kept, ["limits", "no-todo"]);
    assert!(retain_introduced(
        found.clone(),
        &ChangedLines {
            ranges: vec![],
            base_line_count: Some(5)
        }
    )
    .is_empty());
    assert_eq!(retain_introduced(found, &ChangedLines::new_file()).len(), 3);
}

#[test]
fn a_file_over_its_limit_counts_only_when_it_grew() {
    let long: String = (0..20).map(|i| format!("// line {i}\n")).collect();
    let found = check("src/core/long.rs", &long, None);
    assert!(found.iter().any(|v| v.file_wide()));
    let shrank = ChangedLines {
        ranges: vec![(1, 1)],
        base_line_count: Some(25),
    };
    assert!(retain_introduced(found.clone(), &shrank)
        .iter()
        .all(|v| !v.file_wide()));
    let grew = ChangedLines {
        ranges: vec![(20, 20)],
        base_line_count: Some(19),
    };
    assert!(retain_introduced(found, &grew)
        .iter()
        .any(|v| v.file_wide()));
}

#[test]
fn rules_style_and_detectors_are_read_per_file() {
    let guidelines = guidelines();
    let ui: Vec<&str> = guidelines
        .rules_for("src/ui/a.rs")
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(
        ui,
        ["no-unwrap", "no-todo", "ui-not-core", "small-ui", "words"]
    );
    assert_eq!(guidelines.style().for_path("src/ui/a.rs").len(), 2);
    assert_eq!(guidelines.style().for_path("src/core/a.rs").len(), 1);
    assert_eq!(guidelines.detectors().large_method.threshold, 40);
    assert_eq!(guidelines.limits_for("src/ui/a.rs").max_file_lines, Some(4));
}

#[test]
fn every_mistake_in_the_file_is_reported_at_once() {
    let errors = Guidelines::parse(
        Some(
            r#"{"version": 2, "rules": [
          {"id": "a", "kind": "pattern", "regex": "("},
          {"id": "a", "kind": "pattern", "query": "(nope", "lang": "rust"},
          {"id": "b", "kind": "boundary", "must_not_import": []},
          {"id": "c", "kind": "metric"},
          {"id": "d", "kind": "pattern", "query": "(identifier)", "lang": "cobol"}
        ]}"#,
        ),
        None,
    )
    .err()
    .expect("invalid");
    let text: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(errors.len(), 8, "{text:#?}");
    assert!(text[0].contains("version must be 1"));
    assert!(text
        .iter()
        .any(|t| t.contains("'cobol' is not a known language")));
    assert!(Guidelines::parse(Some("{ nope"), None).is_err());
    assert!(Guidelines::parse(None, None).unwrap().is_empty());
}

#[test]
fn published_json_schema_is_current() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schema/guidelines-v1.schema.json"
    );
    let generated = serde_json::to_string_pretty(&guidelines_json_schema()).unwrap() + "\n";
    if std::env::var_os("RUSTY_UPDATE_SCHEMAS").is_some() {
        std::fs::write(path, &generated).expect("write schema");
        return;
    }
    let published = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        published == generated,
        "schema/guidelines-v1.schema.json is out of date; \
         regenerate with RUSTY_UPDATE_SCHEMAS=1 cargo test -p harness-guidelines published_json_schema"
    );
}

#[test]
fn a_misspelt_field_in_a_rule_is_refused() {
    let typo = r#"{"version": 1, "rules": [{"id": "a", "kind": "pattern", "regx": "x"}]}"#;
    assert!(Guidelines::parse(Some(typo), None).is_err());
    let limits = r#"{"version": 1, "rules": [{"id": "a", "kind": "metric", "max_lines": 3}]}"#;
    assert!(Guidelines::parse(Some(limits), None).is_err());
}
