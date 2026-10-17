mod support;

use harness_findings::{Confidence, FindingKind, Severity};
use support::*;

fn find<'a>(set: &'a harness_findings::FindingSet, id: &str) -> &'a harness_findings::Finding {
    set.findings
        .iter()
        .find(|f| f.id.0 == id)
        .unwrap_or_else(|| panic!("no finding {id}; have {:#?}", ids(set)))
}

#[tokio::test]
async fn every_rule_reports_its_true_positives_with_documented_severity_and_confidence() {
    let monorepo = monorepo().await;
    let service = service(&monorepo, test_config(), true);
    let set = analyze(&service).await;
    assert!(
        set.summary.skipped_rules.is_empty(),
        "{:?}",
        set.summary.skipped_rules
    );

    // Measurable.
    let cycle = find(&set, "import-cycle:engine/src/a.rs");
    assert_eq!(
        (cycle.kind, cycle.severity, cycle.confidence),
        (FindingKind::Measurable, Severity::Medium, Confidence::High)
    );
    assert_eq!(cycle.impact, 2);
    assert!(cycle.interpretation.is_none());
    let cross = find(&set, "import-cycle:alpha/src/index.ts");
    assert_eq!(
        cross.severity,
        Severity::High,
        "a cycle across packages is more serious"
    );
    assert_eq!(cross.packages.len(), 2);
    assert_eq!(
        find(&set, "import-cycle:web/src/p.ts").severity,
        Severity::Medium
    );
    let type_cycle = find(&set, "type-only-import-cycle:web/src/x.ts+types");
    assert_eq!(type_cycle.severity, Severity::Info);

    let undeclared = find(
        &set,
        "undeclared-dependency:engine/Cargo.toml:engine|missing_crate",
    );
    assert_eq!(
        (undeclared.severity, undeclared.confidence),
        (Severity::Medium, Confidence::Medium)
    );
    assert_eq!(
        undeclared.evidence.manifests,
        vec![path("engine/Cargo.toml")]
    );
    find(&set, "undeclared-dependency:web/package.json:web|axios");
    let workspace = find(
        &set,
        "undeclared-workspace-dependency:web/package.json:web->lib/package.json:@acme/lib",
    );
    assert_eq!(
        (workspace.severity, workspace.confidence),
        (Severity::Medium, Confidence::High)
    );

    let unattached = find(&set, "unattached-file:engine/src/orphan.rs");
    assert_eq!(
        (unattached.severity, unattached.confidence),
        (Severity::Low, Confidence::Medium)
    );

    let boundary = find(&set, "boundary-violation:ui-isolation|app/src/ui.rs");
    assert_eq!(
        (boundary.severity, boundary.confidence),
        (Severity::High, Confidence::High)
    );
    assert!(boundary.rationale.contains("UI layer"));

    // Candidates are labelled and carry the standing caveat.
    let quiet = find(&set, "unreferenced-file:web/src/legacy.ts");
    assert_eq!(
        (quiet.kind, quiet.severity, quiet.confidence),
        (FindingKind::Candidate, Severity::Low, Confidence::Medium)
    );
    assert!(quiet
        .interpretation
        .as_deref()
        .unwrap()
        .contains("not evidence"));
    let changing = find(&set, "unreferenced-file:web/src/util.ts");
    assert_eq!(
        changing.confidence,
        Confidence::Low,
        "history shows the file is still being edited"
    );

    let coupling = find(&set, "implicit-coupling:engine/src/a.rs|web/src/util.ts");
    assert_eq!(
        (coupling.kind, coupling.severity, coupling.confidence),
        (FindingKind::Candidate, Severity::Info, Confidence::High)
    );
    let git = coupling.evidence.git.as_ref().unwrap();
    assert_eq!(
        (git.support, git.confidence_per_mille),
        (Some(10), Some(1000))
    );
    assert_eq!(git.example_commits.len(), 3);
    assert!(coupling.git_snapshot.is_some());

    let hotspot = find(&set, "change-hotspot:engine/src/a.rs");
    assert_eq!(
        (hotspot.severity, hotspot.confidence),
        (Severity::Low, Confidence::Medium)
    );
    assert_eq!(hotspot.evidence.git.as_ref().unwrap().commits, Some(10));

    let unused_rust = find(
        &set,
        "unused-declared-dependency:engine/Cargo.toml:engine|unused_crate",
    );
    assert_eq!(
        (unused_rust.severity, unused_rust.confidence),
        (Severity::Low, Confidence::Low)
    );
    find(
        &set,
        "unused-declared-dependency:web/package.json:web|lodash",
    );
}

#[tokio::test]
async fn look_alikes_are_not_reported() {
    let monorepo = monorepo().await;
    let set = analyze(&service(&monorepo, test_config(), true)).await;
    let all = ids(&set);
    let has = |needle: &str| all.iter().any(|id| id.contains(needle));

    assert!(!has("Local"), "CamelCase names in scope are not crates");
    assert!(!has("|serde"), "a declared and imported dependency is fine");
    assert!(!has("|react"));
    assert!(
        !has("tempfile") && !has("|cc"),
        "dev and build dependencies are ignored"
    );
    assert!(
        !has("vitest") && !has("@types/node"),
        "dev dependencies and type stubs are ignored"
    );
    assert!(
        !has("unreferenced-file:engine/tests"),
        "tests are not dead code"
    );
    assert!(
        !has("unreferenced-file:engine/src/bin"),
        "binaries are entry points"
    );
    assert!(!has("util.test.ts"), "test files are not unreferenced");
    assert!(
        !has("unreferenced-file:web/scripts"),
        "scripts are entry points"
    );
    assert!(
        !has("unreferenced-file:web/src/index.ts") && !has("unreferenced-file:lib/src/index.ts"),
        "declared entry points"
    );
    assert!(!has("unreferenced-file:app/src/main.rs"), "source roots");
    assert!(
        !has("unreferenced-file:engine/src/orphan.rs"),
        "unattached files get their own finding"
    );
    assert!(
        !has("undeclared-workspace-dependency:ok"),
        "a declared sibling is fine"
    );
    assert!(
        !has("undeclared-workspace-dependency:app"),
        "app declares engine"
    );
    assert!(
        !all.iter()
            .any(|id| id.starts_with("boundary-violation") && id.contains("main.rs")),
        "imports outside the rule's scope"
    );
    assert!(
        !has("|unused_crate") || has("unused-declared-dependency"),
        "sanity"
    );
    assert!(
        !has("implicit-coupling:engine/src/a.rs|engine/src/b.rs"),
        "files joined by imports are not implicitly coupled"
    );
}

#[tokio::test]
async fn an_empty_policy_reports_no_boundary_violations() {
    let monorepo = monorepo().await;
    let set = analyze(&service(
        &monorepo,
        harness_findings::FindingsConfig::default(),
        true,
    ))
    .await;
    assert!(!ids(&set)
        .iter()
        .any(|id| id.starts_with("boundary-violation")));
    assert!(set.summary.skipped_rules.is_empty());
}
