use std::fs;

use harness_code_analysis::{CallGraph, DefaultCallGraph, FindCallablesRequest, RefreshRequest};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn resolves_unique_callable_across_files() {
    let root = std::env::temp_dir().join(format!("call-graph-cross-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("caller.py"),
        "from helper import helper\ndef caller():\n    helper()\n",
    )
    .unwrap();
    fs::write(root.join("helper.py"), "def helper():\n    pass\n").unwrap();

    let graph = DefaultCallGraph::new(&root);
    let snapshot = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    let callables = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(snapshot.clone()),
                language: None,
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
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
                snapshot,
                caller: caller.id.clone(),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(calls.page.items.iter().any(|call| call.target.is_some()));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cross_file_calls_require_static_imports() {
    let root = std::env::temp_dir().join(format!("call-graph-unimported-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("caller.py"), "def caller():\n    helper()\n").unwrap();
    fs::write(root.join("helper.py"), "def helper():\n    pass\n").unwrap();
    let graph = DefaultCallGraph::new(&root);
    let snapshot = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    let callables = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(snapshot.clone()),
                language: None,
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
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
                snapshot,
                caller: caller.id.clone(),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        calls.page.items[0].resolution,
        harness_code_analysis::Resolution::Unresolved
    );
    assert_eq!(
        calls.page.items[0].evidence.as_ref().unwrap().rule_id,
        "unresolved-name"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cursors_are_bound_to_their_snapshot() {
    let root = std::env::temp_dir().join(format!("call-graph-cursor-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("one.rs"), "fn one() {}\nfn two() {}\n").unwrap();
    let graph = DefaultCallGraph::new(&root);
    let first = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    let page = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(first),
                language: None,
                cursor: None,
                limit: 1,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    fs::write(
        root.join("one.rs"),
        "fn one() {}\nfn two() {}\nfn three() {}\n",
    )
    .unwrap();
    let second = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
        .snapshot;
    let error = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(second),
                language: None,
                cursor: page.page.next_cursor,
                limit: 1,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        harness_code_analysis::CallGraphError::InvalidRequest(_)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_utf8_is_partial_and_diagnostic() {
    let root = std::env::temp_dir().join(format!("call-graph-invalid-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("broken.rs"), [b'f', b'n', b' ', 0xff, b'\n']).unwrap();
    let graph = DefaultCallGraph::new(&root);
    let response = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap();
    assert!(response.metadata.partial);
    assert!(!response.metadata.diagnostics.is_empty());
    fs::remove_dir_all(root).unwrap();
}
