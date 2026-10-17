mod support;

use std::sync::Arc;

use harness_findings::{
    AnalyzeRequest, Confidence, DefaultFindingsAnalyzer, Draft, Evidence, FindingId, FindingKind,
    FindingRule, FindingSetId, FindingsAnalysis, FindingsConfig, FindingsError, FindingsInput,
    FindingsListQuery, FindingsQueryError, FindingsService, FsSourceTextReader, Severity,
    Suppression,
};
use support::*;
use tokio_util::sync::CancellationToken;

fn query(set: &FindingSetId, limit: usize) -> FindingsListQuery {
    FindingsListQuery {
        set: set.clone(),
        rules: vec![],
        kind: None,
        min_severity: None,
        min_confidence: None,
        package: None,
        file: None,
        include_suppressed: false,
        cursor: None,
        limit,
    }
}

#[tokio::test]
async fn findings_rank_by_kind_severity_confidence_impact_then_id() {
    let monorepo = monorepo().await;
    let set = analyze(&service(&monorepo, test_config(), true)).await;
    let keys = set
        .findings
        .iter()
        .map(|f| f.priority_key())
        .collect::<Vec<_>>();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "the list must already be in documented order");

    let first_candidate = set
        .findings
        .iter()
        .position(|f| f.kind == FindingKind::Candidate)
        .unwrap();
    assert!(set.findings[..first_candidate]
        .iter()
        .all(|f| f.kind == FindingKind::Measurable));
    assert_eq!(
        ids(&set).into_iter().take(2).collect::<Vec<_>>(),
        vec![
            "boundary-violation:ui-isolation|app/src/ui.rs",
            "import-cycle:alpha/src/index.ts"
        ],
        "high severity, high confidence measurable findings come first"
    );
    // Within candidates, severity then confidence decide: low-severity before info.
    let order = set.findings[first_candidate..]
        .iter()
        .map(|f| (f.severity, f.confidence))
        .collect::<Vec<_>>();
    assert!(order.windows(2).all(|w| w[0] <= w[1]));
}

#[tokio::test]
async fn locations_resolve_to_line_and_column_and_flag_files_that_changed() {
    let monorepo = monorepo().await;
    let service = service(&monorepo, test_config(), true);
    let set = analyze(&service).await;
    let cycle = set
        .findings
        .iter()
        .find(|f| f.id.0 == "import-cycle:engine/src/a.rs")
        .unwrap();
    assert_eq!(cycle.locations.len(), 2);
    for location in &cycle.locations {
        assert_eq!(
            (location.line, location.column, location.stale),
            (Some(1), Some(5), false),
            "{location:?}"
        );
    }
    assert!(set
        .findings
        .iter()
        .find(|f| f.id.0.starts_with("unattached-file"))
        .unwrap()
        .locations
        .is_empty());

    // Edit a.rs after the snapshot: its locations keep the byte span but lose line and column.
    write(
        monorepo.dir.path(),
        "engine/src/a.rs",
        "// new first line\nuse crate::b::B;\n",
    );
    let changed = analyze(&service).await;
    let cycle = changed
        .findings
        .iter()
        .find(|f| f.id.0 == "import-cycle:engine/src/a.rs")
        .unwrap();
    let in_a = cycle
        .locations
        .iter()
        .find(|l| l.file == path("engine/src/a.rs"))
        .unwrap();
    assert!(in_a.stale && in_a.line.is_none() && in_a.end > in_a.start);
    let in_b = cycle
        .locations
        .iter()
        .find(|l| l.file == path("engine/src/b.rs"))
        .unwrap();
    assert!(!in_b.stale && in_b.line == Some(1));
}

#[tokio::test]
async fn suppressions_move_findings_out_of_the_list_but_never_hide_them() {
    let monorepo = monorepo().await;
    let config = FindingsConfig {
        suppressions: vec![Suppression {
            rule: "unused-declared-dependency".into(),
            key: "web/package.json:web|*".into(),
            reason: "used by the build tooling".into(),
        }],
        ..test_config()
    };
    let service = service(&monorepo, config, true);
    let set = analyze(&service).await;
    assert!(!ids(&set).iter().any(|id| id.contains("|lodash")));
    assert_eq!(set.summary.suppressed, 1);
    assert_eq!(set.suppressed[0].reason, "used by the build tooling");
    assert_eq!(
        set.suppressed[0].finding.id.0,
        "unused-declared-dependency:web/package.json:web|lodash"
    );

    let mut with_suppressed = query(&set.id, 100);
    with_suppressed.include_suppressed = true;
    let page = service.list(&with_suppressed).unwrap();
    let entry = page
        .entries
        .iter()
        .find(|e| e.finding.id.0.contains("|lodash"))
        .unwrap();
    assert_eq!(
        entry.suppressed_reason.as_deref(),
        Some("used by the build tooling")
    );
    assert!(service
        .list(&query(&set.id, 100))
        .unwrap()
        .entries
        .iter()
        .all(|e| e.suppressed_reason.is_none()));

    // A suppression must say why.
    let reasonless = FindingsConfig {
        suppressions: vec![Suppression {
            rule: "import-cycle".into(),
            key: "x".into(),
            reason: " ".into(),
        }],
        ..FindingsConfig::default()
    };
    assert!(matches!(
        DefaultFindingsAnalyzer::standard(reasonless),
        Err(FindingsError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn rules_with_missing_inputs_are_skipped_and_say_so() {
    let monorepo = monorepo().await;
    let without_git = analyze(&service(&monorepo, test_config(), false)).await;
    let skipped = without_git
        .summary
        .skipped_rules
        .iter()
        .map(|s| s.rule.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(skipped, vec!["implicit-coupling", "change-hotspot"]);
    assert!(without_git.summary.skipped_rules[0].reason.contains("Git"));
    assert!(without_git.git_snapshot.is_none());
    let legacy = without_git
        .findings
        .iter()
        .find(|f| f.id.0.ends_with("web/src/legacy.ts"))
        .unwrap();
    assert_eq!(
        legacy.confidence,
        Confidence::Low,
        "without history the unreferenced file stays low confidence"
    );

    let dir = tempfile::tempdir().unwrap();
    write_monorepo(dir.path());
    let ungrouped = Monorepo {
        graph: graph_for(dir.path(), false, None),
        git: monorepo.git.clone(),
        dir,
    };
    ungrouped
        .graph
        .refresh(CancellationToken::new())
        .await
        .unwrap();
    let set = analyze(&service(&ungrouped, test_config(), false)).await;
    let skipped = set
        .summary
        .skipped_rules
        .iter()
        .map(|s| s.rule.id.as_str())
        .collect::<Vec<_>>();
    assert!(skipped.contains(&"import-cycle") && skipped.contains(&"type-only-import-cycle"));
    assert!(set
        .summary
        .skipped_rules
        .iter()
        .any(|s| s.reason.contains("groups")));
    assert!(!ids(&set).iter().any(|id| id.contains("cycle")));
    assert!(
        ids(&set)
            .iter()
            .any(|id| id.starts_with("undeclared-dependency")),
        "other rules still run"
    );
}

#[tokio::test]
async fn partial_import_coverage_lowers_cycle_confidence() {
    let dir = tempfile::tempdir().unwrap();
    write_monorepo(dir.path());
    write(
        dir.path(),
        "web/src/big.ts",
        &format!("// {}\n", "padding ".repeat(1000)),
    );
    let limits = harness_workspace::import_graph::ImportLimits {
        max_file_bytes: 1024,
        ..Default::default()
    };
    let monorepo = Monorepo {
        graph: graph_for(dir.path(), true, Some(limits)),
        git: monorepo().await.git.clone(),
        dir,
    };
    monorepo
        .graph
        .refresh(CancellationToken::new())
        .await
        .unwrap();
    assert!(monorepo.graph.current().unwrap().imports.partial);
    let set = analyze(&service(&monorepo, test_config(), false)).await;
    let cycle = set
        .findings
        .iter()
        .find(|f| f.id.0 == "import-cycle:engine/src/a.rs")
        .unwrap();
    assert_eq!(cycle.confidence, Confidence::Medium);
}

#[tokio::test]
async fn analysis_is_deterministic_and_identity_covers_config_and_rule_versions() {
    let monorepo = monorepo().await;
    let first = analyze(&service(&monorepo, test_config(), true)).await;
    let second = analyze(&service(&monorepo, test_config(), true)).await;
    assert_eq!(first.id, second.id);
    assert_eq!(*first, *second);
    let other = analyze(&service(
        &monorepo,
        FindingsConfig {
            hotspot_min_commits: 11,
            ..test_config()
        },
        true,
    ))
    .await;
    assert_ne!(first.id, other.id);

    struct Versioned(u32);
    impl FindingRule for Versioned {
        fn id(&self) -> &'static str {
            "custom"
        }
        fn version(&self) -> u32 {
            self.0
        }
        fn kind(&self) -> FindingKind {
            FindingKind::Candidate
        }
        fn description(&self) -> &'static str {
            "a substitute rule"
        }
        fn evaluate(&self, input: &FindingsInput<'_>) -> Vec<Draft> {
            vec![Draft {
                key: "one".into(),
                severity: Severity::Low,
                confidence: Confidence::Low,
                title: "t".into(),
                summary: "s".into(),
                rationale: "r".into(),
                subjects: vec![input.repository.imports.files[0].path.clone()],
                packages: vec![],
                impact: 1,
                evidence: Evidence::default(),
                spans: vec![],
            }]
        }
    }
    let with = |version| {
        FindingsService::new(
            monorepo.graph.clone(),
            None,
            Arc::new(
                DefaultFindingsAnalyzer::with_rules(
                    vec![Arc::new(Versioned(version))],
                    FindingsConfig::default(),
                )
                .unwrap(),
            ),
            Arc::new(FsSourceTextReader::new(monorepo.dir.path())),
        )
    };
    let v1 = analyze(&with(1)).await;
    let v2 = analyze(&with(2)).await;
    assert_ne!(v1.id, v2.id, "a new rule version is a new finding set");
    assert_eq!(v1.findings[0].id.0, "custom:one");
    assert_eq!(
        (v1.findings[0].rule.id.as_str(), v1.findings[0].rule.version),
        ("custom", 1)
    );
    assert!(
        v1.findings[0].interpretation.is_some(),
        "candidates always carry the caveat"
    );
}

#[tokio::test]
async fn per_rule_bounds_keep_the_highest_ranked_and_report_truncation() {
    let monorepo = monorepo().await;
    let set = analyze(&service(
        &monorepo,
        FindingsConfig {
            max_findings_per_rule: 1,
            ..test_config()
        },
        true,
    ))
    .await;
    assert!(set
        .summary
        .truncated_rules
        .contains(&"undeclared-dependency".to_owned()));
    let undeclared = ids(&set)
        .into_iter()
        .filter(|id| id.starts_with("undeclared-dependency:"))
        .collect::<Vec<_>>();
    assert_eq!(
        undeclared,
        vec!["undeclared-dependency:engine/Cargo.toml:engine|missing_crate"]
    );
}

#[tokio::test]
async fn queries_filter_page_explain_and_reject_bad_input() {
    let monorepo = monorepo().await;
    let service = service(&monorepo, test_config(), true);
    let set = analyze(&service).await;
    let page = |q: FindingsListQuery| service.list(&q).unwrap();

    let candidates = page(FindingsListQuery {
        kind: Some(FindingKind::Candidate),
        ..query(&set.id, 100)
    });
    assert!(
        candidates
            .entries
            .iter()
            .all(|e| e.finding.kind == FindingKind::Candidate)
            && !candidates.entries.is_empty()
    );
    let severe = page(FindingsListQuery {
        min_severity: Some(Severity::High),
        ..query(&set.id, 100)
    });
    assert_eq!(severe.entries.len(), 2);
    let confident = page(FindingsListQuery {
        min_confidence: Some(Confidence::High),
        kind: Some(FindingKind::Candidate),
        ..query(&set.id, 100)
    });
    assert!(confident
        .entries
        .iter()
        .all(|e| e.finding.confidence == Confidence::High));
    let by_rule = page(FindingsListQuery {
        rules: vec!["import-cycle".into()],
        ..query(&set.id, 100)
    });
    assert_eq!(by_rule.entries.len(), 3);
    let by_file = page(FindingsListQuery {
        file: Some(path("engine/src/a.rs")),
        ..query(&set.id, 100)
    });
    assert!(by_file
        .entries
        .iter()
        .any(|e| e.finding.id.0.starts_with("import-cycle")));
    assert!(by_file
        .entries
        .iter()
        .all(|e| e.finding.subjects.contains(&path("engine/src/a.rs"))));
    let engine =
        harness_workspace::repository_intelligence::PackageId("engine/Cargo.toml:engine".into());
    assert!(
        page(FindingsListQuery {
            package: Some(engine),
            ..query(&set.id, 100)
        })
        .entries
        .len()
            >= 3
    );

    // Paging yields each finding once, in ranked order.
    let mut collected = Vec::new();
    let mut paged = query(&set.id, 3);
    loop {
        let result = page(paged.clone());
        collected.extend(result.entries.into_iter().map(|e| e.finding.id.0));
        match result.next_cursor {
            Some(next) => paged.cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, ids(&set));

    // Typed errors instead of silent resets.
    let first = page(query(&set.id, 1));
    let cursor = first.next_cursor.unwrap();
    let mut foreign = cursor.clone();
    foreign.set = FindingSetId("other".into());
    assert_eq!(
        service
            .list(&FindingsListQuery {
                cursor: Some(foreign),
                ..query(&set.id, 1)
            })
            .unwrap_err(),
        FindingsQueryError::ForeignCursor
    );
    assert_eq!(
        service
            .list(&FindingsListQuery {
                cursor: Some(cursor),
                kind: Some(FindingKind::Candidate),
                ..query(&set.id, 1)
            })
            .unwrap_err(),
        FindingsQueryError::InvalidCursor
    );
    assert!(matches!(
        service.list(&query(&set.id, 0)),
        Err(FindingsQueryError::InvalidLimit { .. })
    ));
    assert!(matches!(
        service.list(&query(&FindingSetId("gone".into()), 1)),
        Err(FindingsQueryError::SetNotFound(_))
    ));
    assert!(matches!(
        service.finding(&set.id, &FindingId("nope".into())),
        Err(FindingsQueryError::FindingNotFound(_))
    ));
    assert_eq!(service.summary(&set.id).unwrap(), set.summary);
    assert_eq!(service.current().unwrap().id, set.id);

    // Explain expands evidence into the actual import edges and names the rule.
    let id = FindingId("import-cycle:engine/src/a.rs".into());
    let explanation = service.explain(&set.id, &id).unwrap();
    assert_eq!(explanation.import_sites.len(), 2);
    assert!(explanation.repository_snapshot_available);
    assert!(explanation
        .rule_description
        .unwrap()
        .contains("mutually reachable"));
}

#[tokio::test]
async fn analysis_failures_are_typed_and_cancellation_publishes_nothing() {
    let monorepo = monorepo().await;
    let service = service(&monorepo, test_config(), true);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        service.analyze(AnalyzeRequest::default(), cancel).await,
        Err(FindingsError::Cancelled)
    ));
    assert!(service.current().is_none());

    let missing = AnalyzeRequest {
        repository_snapshot: Some(harness_workspace::repository_intelligence::SnapshotId(
            "gone".into(),
        )),
        ..AnalyzeRequest::default()
    };
    assert!(matches!(
        service.analyze(missing, CancellationToken::new()).await,
        Err(FindingsError::RepositorySnapshotNotFound(_))
    ));
    let missing_git = AnalyzeRequest {
        git_snapshot: Some(harness_git_analysis::GitSnapshotId("gone".into())),
        use_git: true,
        ..AnalyzeRequest::default()
    };
    assert!(matches!(
        service.analyze(missing_git, CancellationToken::new()).await,
        Err(FindingsError::GitSnapshotNotFound(_))
    ));

    let dir = tempfile::tempdir().unwrap();
    write_monorepo(dir.path());
    let unrefreshed = Monorepo {
        graph: graph_for(dir.path(), true, None),
        git: monorepo.git.clone(),
        dir,
    };
    let fresh = self::service(&unrefreshed, test_config(), false);
    assert!(matches!(
        fresh
            .analyze(AnalyzeRequest::default(), CancellationToken::new())
            .await,
        Err(FindingsError::NoRepositorySnapshot)
    ));
}

/// Registers `web/src/legacy.ts` (otherwise an unreferenced candidate) as a route handler.
struct LegacyRoute;

impl harness_workspace::framework_relations::FrameworkAdapter for LegacyRoute {
    fn id(&self) -> &'static str {
        "legacy-router"
    }
    fn version(&self) -> u32 {
        1
    }
    fn capability(&self) -> harness_workspace::framework_relations::FrameworkCapability {
        harness_workspace::framework_relations::FrameworkCapability {
            framework: "legacy-router".into(),
            name: "Legacy router".into(),
            adapter_version: 1,
            kinds: vec![harness_workspace::framework_relations::RelationKind::RouteRegisteredTo],
            handled: vec![],
            unsupported: vec![],
        }
    }
    fn applicability(
        &self,
        package: &harness_workspace::repository_intelligence::Package,
    ) -> Option<String> {
        harness_workspace::framework_relations::declares(package, "lodash")
    }
    fn extract(
        &self,
        package: &harness_workspace::repository_intelligence::Package,
        _: &harness_workspace::framework_relations::FrameworkInput<'_>,
        _: &tokio_util::sync::CancellationToken,
        output: &mut harness_workspace::framework_relations::FrameworkOutput,
    ) -> Result<(), harness_workspace::repository_intelligence::DiscoveryError> {
        use harness_workspace::framework_relations::*;
        let file = path("web/src/legacy.ts");
        output.relate(FrameworkRelation::new(
            "legacy-router",
            Some(&package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: "legacy-route",
                rule_version: 1,
                certainty: Certainty::Declared,
                from: file.clone(),
                to: Some(file.clone()),
                from_symbol: None,
                to_symbol: None,
                label: "GET /legacy".into(),
                evidence: RelationEvidence::new(
                    &file,
                    harness_workspace::import_graph::ByteSpan { start: 0, end: 1 },
                    "router.get('/legacy')",
                ),
            },
        ));
        Ok(())
    }
}

#[tokio::test]
async fn a_file_a_framework_reaches_is_not_an_unreferenced_candidate() {
    let dir = tempfile::tempdir().unwrap();
    write_monorepo(dir.path());
    let graph = graph_with_frameworks(dir.path(), true, None, vec![Arc::new(LegacyRoute)]);
    graph.refresh(CancellationToken::new()).await.unwrap();
    let service = FindingsService::new(
        graph,
        None,
        Arc::new(DefaultFindingsAnalyzer::standard(test_config()).unwrap()),
        Arc::new(FsSourceTextReader::new(dir.path())),
    );
    let with = analyze(&service).await;
    assert!(!ids(&with).contains(&"unreferenced-file:web/src/legacy.ts".to_owned()));
    assert!(
        ids(&with).contains(&"unreferenced-file:web/src/util.ts".to_owned()),
        "other candidates remain"
    );

    let plain = service_for_plain(dir.path()).await;
    let without = analyze(&plain).await;
    assert!(ids(&without).contains(&"unreferenced-file:web/src/legacy.ts".to_owned()));
}

async fn service_for_plain(root: &std::path::Path) -> FindingsService {
    let graph = graph_for(root, true, None);
    graph.refresh(CancellationToken::new()).await.unwrap();
    FindingsService::new(
        graph,
        None,
        Arc::new(DefaultFindingsAnalyzer::standard(test_config()).unwrap()),
        Arc::new(FsSourceTextReader::new(root)),
    )
}
