mod support;
use support::Fixture;

use harness_code_analysis::{
    CallGraph, CallGraphError, CallsFromRequest, CallsToRequest, Direction, FindCallablesRequest,
    LanguageId, Resolution, SymbolId, TraversalLimit, TraversalRequest,
};
use tokio_util::sync::CancellationToken;

const GRAPH: &str = "def leaf():\n    pass\ndef pick():\n    pass\ndef pick():\n    pass\ndef mid():\n    leaf()\n    pick()\n    missing()\n    leaf()\ndef top():\n    mid()\ndef ping():\n    pong()\ndef pong():\n    ping()\n";

fn traversal(
    f: &Fixture,
    roots: &[&str],
    depth: u32,
    limit: u32,
    direction: Direction,
) -> TraversalRequest {
    TraversalRequest {
        snapshot: f.snapshot.clone(),
        roots: roots
            .iter()
            .map(|name| f.callable("g.py", name).id.clone())
            .collect(),
        depth,
        limit,
        direction,
        resolutions: Vec::new(),
    }
}

#[tokio::test]
async fn outgoing_calls_keep_unresolved_sites_and_filter_by_resolution() {
    let f = Fixture::new(&[("g.py", GRAPH)]).await;
    let mid = f.callable("g.py", "mid").clone();
    let all = f.calls(&mid).await;
    assert_eq!(all.len(), 4, "repeated and unresolved calls are retained");
    let request = |resolutions: Vec<Resolution>| CallsFromRequest {
        snapshot: f.snapshot.clone(),
        caller: mid.id.clone(),
        resolutions,
        cursor: None,
        limit: 50,
    };
    for (filter, expected) in [
        (vec![Resolution::Resolved], 2),
        (vec![Resolution::Possible], 1),
        (vec![Resolution::Unresolved], 1),
        (vec![Resolution::Resolved, Resolution::Possible], 3),
    ] {
        let page = f
            .graph
            .calls_from(request(filter.clone()), CancellationToken::new())
            .await
            .unwrap()
            .page;
        assert_eq!(page.items.len(), expected, "{filter:?}");
    }
}

#[tokio::test]
async fn incoming_calls_include_possible_candidates_but_not_unresolved() {
    let f = Fixture::new(&[("g.py", GRAPH)]).await;
    let picks: Vec<_> = f.callables.iter().filter(|c| c.name == "pick").collect();
    assert_eq!(picks.len(), 2);
    for pick in picks {
        let incoming = f.callers_of(pick).await;
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].resolution, Resolution::Possible);
        assert!(incoming[0].candidates.contains(&pick.id));
    }
    let leaf = f.callable("g.py", "leaf").clone();
    let incoming = f.callers_of(&leaf).await;
    assert_eq!(
        incoming.len(),
        2,
        "both call sites are preserved with their spans"
    );
    let only_possible = f
        .graph
        .calls_to(
            CallsToRequest {
                snapshot: f.snapshot.clone(),
                target: leaf.id.clone(),
                resolutions: vec![Resolution::Possible],
                cursor: None,
                limit: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(only_possible.page.items.is_empty());
}

#[tokio::test]
async fn invalid_requests_and_missing_snapshots_use_documented_errors() {
    let f = Fixture::new(&[("g.py", GRAPH)]).await;
    let mid = f.callable("g.py", "mid").clone();
    let from = |caller: SymbolId, limit: u32, snapshot| CallsFromRequest {
        snapshot,
        caller,
        resolutions: Vec::new(),
        cursor: None,
        limit,
    };
    fn error<T>(result: Result<T, CallGraphError>) -> CallGraphError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }
    let zero = f
        .graph
        .calls_from(
            from(mid.id.clone(), 0, f.snapshot.clone()),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(error(zero), CallGraphError::InvalidRequest(_)));
    let unknown = f
        .graph
        .calls_from(
            from(SymbolId("nope".into()), 5, f.snapshot.clone()),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(error(unknown), CallGraphError::InvalidRequest(_)));
    let missing = f
        .graph
        .calls_from(
            from(
                mid.id.clone(),
                5,
                harness_code_analysis::SnapshotId("snap-gone".into()),
            ),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        error(missing),
        CallGraphError::SnapshotNotFound(_)
    ));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = f
        .graph
        .calls_from(from(mid.id.clone(), 5, f.snapshot.clone()), cancelled)
        .await;
    assert_eq!(error(result), CallGraphError::Cancelled);
    let deep = f
        .graph
        .traverse(
            traversal(&f, &["mid"], 1000, 5, Direction::Callees),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(error(deep), CallGraphError::LimitExceeded(_)));
    let mut empty = traversal(&f, &["mid"], 1, 5, Direction::Callees);
    empty.roots.clear();
    assert!(matches!(
        error(f.graph.traverse(empty, CancellationToken::new()).await),
        CallGraphError::InvalidRequest(_)
    ));
}

#[tokio::test]
async fn pagination_is_ordered_bounded_and_bound_to_its_snapshot() {
    let body: String = (0..7).map(|i| format!("def f{i}():\n    pass\n")).collect();
    let mut f = Fixture::new(&[("p.py", body.as_str())]).await;
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    let mut stale_cursor = None;
    loop {
        let page = f
            .graph
            .find_callables(
                FindCallablesRequest {
                    snapshot: Some(f.snapshot.clone()),
                    language: Some(LanguageId::Python),
                    name: None,
                    path: Some("p.py".into()),
                    cursor: cursor.clone(),
                    limit: 3,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .page;
        pages += 1;
        assert!(page.items.len() <= 3);
        assert_eq!(page.metadata.truncated, page.next_cursor.is_some());
        seen.extend(page.items.iter().map(|c| c.name.clone()));
        stale_cursor = page.next_cursor.clone().or(stale_cursor);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen,
        ["f0", "f1", "f2", "f3", "f4", "f5", "f6"],
        "source order, no duplicates"
    );
    // A cursor from another generation is rejected rather than silently reinterpreted.
    let old = f.snapshot.clone();
    std::fs::write(
        f.root.join("p.py"),
        format!("{body}def extra():\n    pass\n"),
    )
    .unwrap();
    f.refresh_again().await;
    assert_ne!(old, f.snapshot);
    let result = f
        .graph
        .find_callables(
            FindCallablesRequest {
                snapshot: Some(f.snapshot.clone()),
                language: None,
                name: None,
                path: None,
                cursor: stale_cursor,
                limit: 3,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(CallGraphError::InvalidRequest(_))));
}

#[tokio::test]
async fn find_callables_filters_by_name_and_qualified_suffix() {
    let src = "class A:\n    def run(self):\n        pass\nclass B:\n    def run(self):\n        pass\ndef run():\n    pass\n";
    let f = Fixture::new(&[("m.py", src)]).await;
    let find = |name: &str| FindCallablesRequest {
        snapshot: Some(f.snapshot.clone()),
        language: None,
        name: Some(name.into()),
        path: None,
        cursor: None,
        limit: 10,
    };
    let by_simple = f
        .graph
        .find_callables(find("run"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(by_simple.page.items.len(), 3);
    let by_qualified = f
        .graph
        .find_callables(find("A.run"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(by_qualified.page.items.len(), 1);
    assert_eq!(by_qualified.page.items[0].qualified_name, "A.run");
}

#[tokio::test]
async fn traversal_handles_cycles_depth_limits_and_directions() {
    let f = Fixture::new(&[("g.py", GRAPH)]).await;
    // Mutual recursion terminates and reports each node and edge once.
    let cycle = f
        .graph
        .traverse(
            traversal(&f, &["ping"], 10, 50, Direction::Callees),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(cycle.nodes.items.len(), 2);
    assert_eq!(cycle.edges.items.len(), 2);
    assert!(cycle.truncated_by.is_empty());

    // top -> mid -> {leaf, pick*}: depth 1 stops before leaf and says so.
    let shallow = f
        .graph
        .traverse(
            traversal(&f, &["top"], 1, 50, Direction::Callees),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let names: Vec<_> = shallow
        .nodes
        .items
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names, ["mid", "top"]);
    assert_eq!(shallow.truncated_by, [TraversalLimit::Depth]);
    let deep = f
        .graph
        .traverse(
            traversal(&f, &["top"], 2, 50, Direction::Callees),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(deep.truncated_by.is_empty());
    assert_eq!(
        deep.nodes.items.len(),
        5,
        "top, mid, leaf and both pick candidates"
    );
    // Possible candidates are followed; unresolved sites appear as edges but lead nowhere.
    assert!(deep
        .edges
        .items
        .iter()
        .any(|e| e.resolution == Resolution::Possible));
    assert!(deep
        .edges
        .items
        .iter()
        .any(|e| e.resolution == Resolution::Unresolved));

    // Restricting to resolved edges drops the possible candidates.
    let mut resolved_only = traversal(&f, &["top"], 2, 50, Direction::Callees);
    resolved_only.resolutions = vec![Resolution::Resolved];
    let result = f
        .graph
        .traverse(resolved_only, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.nodes.items.len(), 3);

    // Callers direction walks backwards.
    let callers = f
        .graph
        .traverse(
            traversal(&f, &["leaf"], 5, 50, Direction::Callers),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let names: Vec<_> = callers
        .nodes
        .items
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names, ["leaf", "mid", "top"]);
    let both = f
        .graph
        .traverse(
            traversal(&f, &["mid"], 1, 50, Direction::Both),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let names: Vec<_> = both.nodes.items.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"top") && names.contains(&"leaf"));
}

#[tokio::test]
async fn traversal_reports_the_page_limit_that_truncated_it() {
    let f = Fixture::new(&[("g.py", GRAPH)]).await;
    let result = f
        .graph
        .traverse(
            traversal(&f, &["top"], 5, 2, Direction::Callees),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.nodes.items.len(), 2);
    assert!(result.truncated_by.contains(&TraversalLimit::NodeLimit));
    assert!(result.nodes.metadata.truncated && result.edges.metadata.truncated);
}

#[tokio::test]
async fn byte_spans_are_utf8_offsets_with_unicode_and_crlf() {
    let src = "# héllo wörld ✓\r\ndef café():\r\n    pass\r\n\r\ndef 日本語():\r\n    café()\r\n";
    let f = Fixture::new(&[("u.py", src)]).await;
    for name in ["café", "日本語"] {
        let symbol = f.callable("u.py", name);
        assert_eq!(
            &src.as_bytes()[symbol.span.start_byte as usize..symbol.span.end_byte as usize],
            name.as_bytes(),
            "half-open UTF-8 byte span of {name}"
        );
    }
    let call = f.site("u.py", "日本語", "café").await;
    assert_eq!(
        &src.as_bytes()[call.span.start_byte as usize..call.span.end_byte as usize],
        "café".as_bytes()
    );
    assert_eq!(call.resolution, Resolution::Resolved);
}
