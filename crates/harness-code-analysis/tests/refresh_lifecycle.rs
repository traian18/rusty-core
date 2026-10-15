mod support;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use harness_code_analysis::{
    CallGraph, CallGraphBuilder, CallGraphError, FindCallablesRequest, InMemorySnapshotRepository,
    RefreshRequest, Resolution, SnapshotRepository,
};
use support::fakes::{CountingAdapter, FlakyRepository, MemorySource};
use support::{write_files, Fixture};
use tokio_util::sync::CancellationToken;

fn root() -> std::path::PathBuf {
    support::temp_root("lifecycle")
}

#[tokio::test]
async fn edits_additions_deletions_and_renames_leave_no_stale_relationships() {
    let files = [
        ("helper.py", "def helper():\n    pass\n"),
        (
            "main.py",
            "from helper import helper\ndef go():\n    helper()\n",
        ),
    ];
    let mut f = Fixture::new(&files).await;
    let first = f.snapshot.clone();
    let helper = f.callable("helper.py", "helper").clone();
    assert_eq!(f.callers_of(&helper).await.len(), 1);

    // Edit: the callee moves within the file; ids follow the new spans.
    std::fs::write(f.root.join("helper.py"), "\n\ndef helper():\n    pass\n").unwrap();
    f.sources
        .insert("helper.py".into(), "\n\ndef helper():\n    pass\n".into());
    f.refresh_again().await;
    assert_ne!(f.snapshot, first, "content change yields a new generation");
    let moved = f.callable("helper.py", "helper").clone();
    assert_ne!(moved.id, helper.id);
    f.assert_resolved("main.py", "go", "helper", "helper.py", "helper")
        .await;
    assert_eq!(f.callers_of(&moved).await.len(), 1);

    // Rename the module: the old import no longer resolves and no edge points anywhere.
    std::fs::rename(f.root.join("helper.py"), f.root.join("helper2.py")).unwrap();
    f.refresh_again().await;
    let stale = f.site("main.py", "go", "helper").await;
    assert_eq!(stale.resolution, Resolution::Unresolved);
    assert!(stale.target.is_none() && stale.candidates.is_empty());

    // Fix the import: the relationship is rebuilt against the new file.
    let fixed = "from helper2 import helper\ndef go():\n    helper()\n";
    std::fs::write(f.root.join("main.py"), fixed).unwrap();
    f.sources.insert("main.py".into(), fixed.into());
    f.refresh_again().await;
    f.assert_resolved("main.py", "go", "helper", "helper2.py", "helper")
        .await;

    // Delete the callee: its callable and the incoming edge disappear.
    std::fs::remove_file(f.root.join("helper2.py")).unwrap();
    f.refresh_again().await;
    assert!(!f.callables.iter().any(|c| c.span.path == "helper2.py"));
    let stale = f.site("main.py", "go", "helper").await;
    assert_eq!(stale.resolution, Resolution::Unresolved);
    assert!(stale.target.is_none());

    // Add a new file: it appears in the next generation only.
    write_files(&f.root, &[("extra.py", "def extra():\n    pass\n")]);
    f.sources
        .insert("extra.py".into(), "def extra():\n    pass\n".into());
    f.refresh_again().await;
    f.callable("extra.py", "extra");
}

#[tokio::test]
async fn unchanged_workspace_reuses_extraction_and_keeps_snapshot_identity() {
    let files = [
        ("a.py", "def a():\n    pass\n"),
        ("b.py", "from a import a\ndef b():\n    a()\n"),
    ];
    let mut f = Fixture::new(&files).await;
    assert_eq!(f.refresh.stats.cache_misses, 2);
    assert_eq!(f.refresh.stats.cache_hits, 0);
    let first = f.snapshot.clone();
    f.refresh_again().await;
    assert_eq!(
        f.refresh.stats.cache_hits, 2,
        "unchanged files reuse extraction facts"
    );
    assert_eq!(f.refresh.stats.cache_misses, 0);
    assert_eq!(
        f.snapshot, first,
        "identity is deterministic for unchanged input"
    );
    // Editing one file re-extracts only that file but re-resolves the whole project.
    std::fs::write(f.root.join("a.py"), "def a():\n    pass\n# edit\n").unwrap();
    f.refresh_again().await;
    assert_eq!(f.refresh.stats.cache_misses, 1);
    assert_eq!(f.refresh.stats.cache_hits, 1);
    f.assert_resolved("b.py", "b", "a", "a.py", "a").await;
}

#[tokio::test]
async fn snapshot_identity_is_stable_across_instances() {
    let files = [("a.py", "def a():\n    pass\n")];
    let one = Fixture::new(&files).await;
    let two = Fixture::new(&files).await;
    assert_eq!(one.snapshot, two.snapshot);
    assert_eq!(one.callables, two.callables);
}

#[tokio::test]
async fn adapter_version_change_invalidates_cache_and_snapshot_identity() {
    let source = MemorySource::with(&[("a.rs", "fn:one fn:two")]);
    let adapter = CountingAdapter::new();
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(source.clone())
        .adapter(adapter.clone())
        .build()
        .unwrap();
    let first = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(adapter.extractions.load(Ordering::SeqCst), 1);
    let again = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(adapter.extractions.load(Ordering::SeqCst), 1, "cache hit");
    assert_eq!(first.snapshot, again.snapshot);
    *adapter.version.lock().unwrap() = "count-2";
    let upgraded = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        adapter.extractions.load(Ordering::SeqCst),
        2,
        "new adapter version re-extracts"
    );
    assert_ne!(
        first.snapshot, upgraded.snapshot,
        "analysis rules are part of the identity"
    );
    assert_eq!(graph.capabilities().languages[0].adapter_version, "count-2");
}

#[tokio::test]
async fn failed_and_cancelled_refreshes_preserve_the_last_usable_snapshot() {
    let source = MemorySource::with(&[("a.rs", "fn:one")]);
    let adapter = CountingAdapter::new();
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(source.clone())
        .adapter(adapter.clone())
        .build()
        .unwrap();
    let good = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();

    source.fail.store(true, Ordering::SeqCst);
    let error = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, CallGraphError::DependencyFailure(_)));
    source.fail.store(false, Ordering::SeqCst);

    source.set(&[("a.rs", "fn:one"), ("b.rs", "fn:two")]);
    *adapter.fail_on.lock().unwrap() = Some("b.rs".into());
    let error = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, CallGraphError::DependencyFailure(_)),
        "fatal adapter failure"
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        graph
            .refresh(RefreshRequest::default(), cancelled)
            .await
            .unwrap_err(),
        CallGraphError::Cancelled
    );

    // The previous generation is still queryable, unchanged.
    let listed = graph
        .find_callables(
            FindCallablesRequest {
                snapshot: Some(good.snapshot.clone()),
                language: None,
                name: None,
                path: None,
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(listed.page.items.len(), 1);
    // Recovery: once the failure clears, the next refresh publishes the new files.
    *adapter.fail_on.lock().unwrap() = None;
    let recovered = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(recovered.snapshot, good.snapshot);
    assert_eq!(recovered.stats.callables, 2);
}

#[tokio::test]
async fn per_file_limit_failures_publish_a_partial_snapshot() {
    let source = MemorySource::with(&[("a.rs", "fn:one"), ("b.rs", "fn:two")]);
    let adapter = CountingAdapter::new();
    *adapter.fail_on.lock().unwrap() = Some("b.rs".into());
    *adapter.fail_with.lock().unwrap() = CallGraphError::LimitExceeded("parser budget".into());
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(source)
        .adapter(adapter)
        .build()
        .unwrap();
    let response = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert!(response.metadata.partial);
    assert_eq!(response.stats.files_skipped, 1);
    assert_eq!(response.stats.limits_triggered, 1);
    assert_eq!(response.stats.callables, 1);
    assert!(response
        .metadata
        .diagnostics
        .iter()
        .any(|d| d.path.as_deref() == Some("b.rs") && d.message.contains("parser budget")));
}

#[tokio::test]
async fn publication_failure_is_all_or_nothing_and_recovers() {
    let source = MemorySource::with(&[("a.rs", "fn:one")]);
    let adapter = CountingAdapter::new();
    let repository = FlakyRepository::new(Arc::new(InMemorySnapshotRepository::new(4).unwrap()));
    let graph = CallGraphBuilder::new(std::env::temp_dir())
        .source(source)
        .adapter(adapter.clone())
        .snapshot_repository(repository.clone())
        .build()
        .unwrap();
    repository.fail_publish.store(true, Ordering::SeqCst);
    assert!(graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .is_err());
    assert!(repository.current().is_none(), "nothing was exposed");
    assert_eq!(adapter.extractions.load(Ordering::SeqCst), 1);
    repository.fail_publish.store(false, Ordering::SeqCst);
    let response = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        adapter.extractions.load(Ordering::SeqCst),
        1,
        "staged extraction was kept for the retry"
    );
    assert_eq!(response.stats.cache_hits, 1);
    assert!(repository.current().is_some());
}

#[tokio::test]
async fn refreshes_are_serialized_and_reads_see_immutable_generations() {
    let source = MemorySource::with(&[("a.rs", "fn:one")]);
    let adapter = CountingAdapter::new();
    let graph = Arc::new(
        CallGraphBuilder::new(std::env::temp_dir())
            .source(source.clone())
            .adapter(adapter)
            .build()
            .unwrap(),
    );
    let first = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();

    // Hold the next refresh open inside discovery.
    source.block_next.store(true, Ordering::SeqCst);
    source.set(&[("a.rs", "fn:one"), ("b.rs", "fn:two")]);
    let slow = tokio::spawn({
        let graph = graph.clone();
        async move {
            graph
                .refresh(RefreshRequest::default(), CancellationToken::new())
                .await
        }
    });
    source.entered.notified().await;

    // A second refresh waits behind the first; queries meanwhile read the published generation.
    let queued = tokio::spawn({
        let graph = graph.clone();
        async move {
            graph
                .refresh(RefreshRequest::default(), CancellationToken::new())
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        source.discoveries.load(Ordering::SeqCst),
        2,
        "queued refresh has not started"
    );
    let listing = graph
        .find_callables(
            FindCallablesRequest {
                snapshot: None,
                language: None,
                name: None,
                path: None,
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(listing.page.metadata.snapshot, first.snapshot);
    assert_eq!(listing.page.items.len(), 1);

    source.gate.notify_one();
    let published = slow.await.unwrap().unwrap();
    let last = queued.await.unwrap().unwrap();
    assert_ne!(published.snapshot, first.snapshot);
    assert_eq!(
        published.snapshot, last.snapshot,
        "same input, same generation"
    );
    assert_eq!(source.discoveries.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelling_while_waiting_for_the_refresh_lock_returns_promptly() {
    let source = MemorySource::with(&[("a.rs", "fn:one")]);
    let graph = Arc::new(
        CallGraphBuilder::new(std::env::temp_dir())
            .source(source.clone())
            .adapter(CountingAdapter::new())
            .build()
            .unwrap(),
    );
    source.block_next.store(true, Ordering::SeqCst);
    let holder = tokio::spawn({
        let graph = graph.clone();
        async move {
            graph
                .refresh(RefreshRequest::default(), CancellationToken::new())
                .await
        }
    });
    source.entered.notified().await;
    let cancel = CancellationToken::new();
    let waiter = tokio::spawn({
        let graph = graph.clone();
        let cancel = cancel.clone();
        async move { graph.refresh(RefreshRequest::default(), cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("prompt")
        .unwrap();
    assert_eq!(result.unwrap_err(), CallGraphError::Cancelled);
    source.gate.notify_one();
    holder.await.unwrap().unwrap();
}

#[tokio::test]
async fn partial_metadata_reports_limits_hashes_and_unresolved_reasons() {
    let mut files = vec![("big.py".to_owned(), "x = 1\n".repeat(200_000))];
    files.push((
        "ok.py".into(),
        "def ok():\n    missing()\n    other.thing()\n".into(),
    ));
    let root = root();
    for (path, text) in &files {
        std::fs::write(root.join(path), text).unwrap();
    }
    let source = harness_code_analysis::FileSystemSource::new(64 * 1024, 4);
    let graph = CallGraphBuilder::new(&root)
        .source(Arc::new(source))
        .build()
        .unwrap();
    let response = graph
        .refresh(RefreshRequest::default(), CancellationToken::new())
        .await
        .unwrap();
    assert!(response.metadata.partial);
    assert!(response.stats.limits_triggered >= 1);
    assert!(response
        .metadata
        .limits
        .iter()
        .any(|l| l.contains("max_file_bytes=65536")));
    assert!(response.metadata.generated_at_unix > 0);
    assert_eq!(
        response.stats.unresolved_by_reason.get("unresolved-name"),
        Some(&1)
    );
    assert_eq!(
        response.stats.unresolved_by_reason.get("dynamic-receiver"),
        Some(&1)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn resolution_labels_are_counted() {
    let f = Fixture::new(&[(
        "a.py",
        "def pick():\n    pass\ndef pick():\n    pass\ndef go():\n    pick()\n    nothing()\n",
    )])
    .await;
    assert_eq!(f.refresh.stats.possible, 1);
    assert_eq!(f.refresh.stats.unresolved, 1);
    assert_eq!(
        f.site("a.py", "go", "pick").await.resolution,
        Resolution::Possible
    );
}
