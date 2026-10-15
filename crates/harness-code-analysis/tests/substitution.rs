//! Components behind the contracts can be replaced without any change to consumer code.
mod support;

use std::sync::Arc;

use harness_code_analysis::{
    CallFact, CallGraph, CallGraphBuilder, CallsFromRequest, FindCallablesRequest, LanguageId,
    LanguageResolver, Outcome, RefreshRequest, Resolution, ResolutionContext, TargetRef,
};
use support::fakes::{CountingAdapter, MemorySource};
use support::Fixture;
use tokio_util::sync::CancellationToken;

/// Resolves every call to the first callable with that name anywhere (deliberately naive).
struct NaiveResolver;

impl LanguageResolver for NaiveResolver {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }
    fn version(&self) -> &'static str {
        "naive-1"
    }
    fn resolve(&self, ctx: &ResolutionContext<'_>, call: &CallFact) -> Outcome {
        for (path, file) in ctx.project.files() {
            if let Some(callable) = file.callables.iter().find(|c| c.name == call.name) {
                return Outcome::Resolved {
                    target: TargetRef {
                        path: path.to_owned(),
                        key: callable.key,
                    },
                    rule: "naive-name",
                    explanation: "first callable with the same name".into(),
                    span: None,
                };
            }
        }
        Outcome::Unresolved {
            rule: "naive-miss",
            explanation: "no callable".into(),
        }
    }
}

async fn edges(graph: &dyn CallGraph) -> Vec<(Resolution, String, String)> {
    let snapshot = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    let callables = graph
        .find_callables(
            FindCallablesRequest {
                snapshot: Some(snapshot.clone()),
                language: None,
                name: None,
                path: None,
                cursor: None,
                limit: 50,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .page
        .items;
    let mut out = Vec::new();
    for caller in &callables {
        let calls = graph
            .calls_from(
                CallsFromRequest {
                    snapshot: snapshot.clone(),
                    caller: caller.id.clone(),
                    resolutions: Vec::new(),
                    cursor: None,
                    limit: 50,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        for call in calls.page.items {
            let evidence = call.evidence.unwrap();
            out.push((call.resolution, evidence.rule_id, evidence.adapter_version));
        }
    }
    out
}

#[tokio::test]
async fn replacing_the_resolver_changes_results_for_unchanged_consumers() {
    let source = MemorySource::with(&[("a.rs", "fn:one call:two fn:two")]);
    let without = CallGraphBuilder::new(std::env::temp_dir())
        .source(source.clone())
        .adapter(CountingAdapter::new())
        .build()
        .unwrap();
    let with = CallGraphBuilder::new(std::env::temp_dir())
        .source(source)
        .adapter(CountingAdapter::new())
        .resolver(Arc::new(NaiveResolver))
        .build()
        .unwrap();
    // The same consumer function drives both implementations through `dyn CallGraph`.
    let a = edges(&without).await;
    let b = edges(&with).await;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].0, Resolution::Unresolved);
    assert_eq!(a[0].1, "no-resolver");
    assert_eq!(b[0].0, Resolution::Resolved);
    assert_eq!(b[0].1, "naive-name");
    assert!(
        b[0].2.ends_with("+naive-1"),
        "evidence carries the substitute's version: {}",
        b[0].2
    );
}

#[tokio::test]
async fn replacing_the_adapter_changes_extraction_for_unchanged_consumers() {
    let source = MemorySource::with(&[("a.rs", "fn:one fn:two fn:three")]);
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(source)
        .adapter(CountingAdapter::new())
        .build()
        .unwrap();
    let response = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.stats.callables, 3);
    assert_eq!(graph.capabilities().languages.len(), 1);
    assert_eq!(graph.capabilities().languages[0].language, LanguageId::Rust);
}

#[tokio::test]
async fn candidate_sets_are_capped_and_the_truncation_is_reported() {
    let body: String = (0..12)
        .map(|_| "def pick():\n    pass\n")
        .collect::<String>()
        + "def go():\n    pick()\n";
    let f = Fixture::new(&[("a.py", body.as_str())]).await;
    let site = f.site("a.py", "go", "pick").await;
    assert_eq!(site.resolution, Resolution::Possible);
    assert_eq!(site.candidates.len(), 8);
    assert!(site.candidates_truncated);
    assert!(site.evidence.unwrap().explanation.contains("truncated"));
}
