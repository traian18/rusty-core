use std::fs;

use harness_code_analysis::{CallGraph, DefaultCallGraph, FindCallablesRequest, RefreshRequest};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn refresh_discovers_callables_and_queries_edges() {
    let root = std::env::temp_dir().join(format!("call-graph-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.rs"),
        "fn caller() {\n callee(); missing();\n}\nfn callee() {}\n",
    )
    .unwrap();

    let graph = DefaultCallGraph::new(&root);
    let refreshed = graph
        .refresh(
            RefreshRequest {
                max_files: Some(10),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let callables = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(refreshed.snapshot.clone()),
                language: None,
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(callables.page.items.len(), 2);
    let caller = callables
        .page
        .items
        .iter()
        .find(|item| item.name == "caller")
        .unwrap();
    let calls = graph
        .calls_from(
            harness_code_analysis::CallsFromRequest {
                resolutions: Vec::new(),
                snapshot: refreshed.snapshot,
                caller: caller.id.clone(),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(calls.page.items.len() >= 2);
    let resolved = calls
        .page
        .items
        .iter()
        .find(|call| call.target.is_some())
        .unwrap();
    assert_eq!(&resolved.span.path, "main.rs");
    assert!(resolved.span.end_byte > resolved.span.start_byte);
    assert_eq!(
        &"fn caller() {\n callee(); missing();\n}\nfn callee() {}\n"
            [resolved.span.start_byte as usize..resolved.span.end_byte as usize],
        "callee"
    );
    assert!(calls.page.items.iter().any(|call| call.target.is_none()));
    assert!(!calls.page.metadata.freshness.is_empty());
    assert!(!calls.page.metadata.limits.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn refresh_keeps_previous_snapshot_until_retention_expires() {
    let root = std::env::temp_dir().join(format!("call-graph-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("main.rs");
    fs::write(&path, "fn first() {}\n").unwrap();
    let graph = DefaultCallGraph::new(&root);
    let first = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    fs::write(&path, "fn second() {}\n").unwrap();
    let second = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;

    assert_ne!(first, second);
    let old = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(first),
                language: None,
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(old.page.items[0].name, "first");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn publication_failure_keeps_staged_extraction_for_retry() {
    use harness_code_analysis::{
        CallGraphBuilder, CallGraphError, GraphSnapshot, SnapshotId, SnapshotRepository,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct FlakyRepository {
        fail: AtomicBool,
        inner: harness_code_analysis::InMemorySnapshotRepository,
    }
    impl SnapshotRepository for FlakyRepository {
        fn current(&self) -> Option<Arc<GraphSnapshot>> {
            self.inner.current()
        }
        fn publish(&self, snapshot: GraphSnapshot) -> Result<(), CallGraphError> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(CallGraphError::DependencyFailure(
                    "injected publication failure".into(),
                ));
            }
            self.inner.publish(snapshot)
        }
        fn get(&self, id: &SnapshotId) -> Result<Arc<GraphSnapshot>, CallGraphError> {
            self.inner.get(id)
        }
    }

    let root = std::env::temp_dir().join(format!("call-graph-publish-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.rs"), "fn work() {}\n").unwrap();
    let repository = Arc::new(FlakyRepository {
        fail: AtomicBool::new(true),
        inner: harness_code_analysis::InMemorySnapshotRepository::new(2).unwrap(),
    });
    let graph = CallGraphBuilder::new(&root)
        .snapshot_repository(repository.clone())
        .build()
        .unwrap();
    assert!(matches!(
        graph
            .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
            .await,
        Err(CallGraphError::DependencyFailure(_))
    ));
    assert!(repository.current().is_none());
    let refreshed = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap();
    let callables = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(refreshed.snapshot.clone()),
                language: None,
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(callables.page.items.len(), 1);
    assert_eq!(callables.page.items[0].name, "work");
    assert!(repository.get(&refreshed.snapshot).is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_refresh_does_not_publish_a_snapshot() {
    let root = std::env::temp_dir().join(format!("call-graph-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.py"), "def work():\n    pass\n").unwrap();
    let graph = DefaultCallGraph::new(&root);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        graph
            .refresh(RefreshRequest { max_files: None }, cancel)
            .await,
        Err(harness_code_analysis::CallGraphError::Cancelled)
    ));
    fs::remove_dir_all(root).unwrap();
}
