use super::*;

#[tokio::test]
async fn snapshot_repository_is_substitutable() {
    let dir = tempdir().unwrap();
    let snapshots = Arc::new(SubstituteSnapshots::new());
    let graph = RepositoryGraphBuilder::new(dir.path())
        .unwrap()
        .discovery(Arc::new(Substitute))
        .snapshot_repository(snapshots.clone())
        .build();

    let published = graph.refresh(CancellationToken::new()).await.unwrap();
    assert_eq!(graph.current().unwrap().id, published.id);
    assert_eq!(graph.get(&published.id).unwrap().id, published.id);
    assert_eq!(snapshots.current().unwrap().id, published.id);
}

#[tokio::test]
async fn repository_graph_refresh_publishes_immutable_snapshots_and_preserves_prior_on_cancel() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("package.json");
    std::fs::write(&manifest, r#"{"name":"first"}"#).unwrap();
    let repository = Arc::new(InMemoryRepositorySnapshotRepository::new(1).unwrap());
    let graph = RepositoryGraphBuilder::new(dir.path())
        .unwrap()
        .snapshot_repository(repository)
        .build();

    let first = graph.refresh(CancellationToken::new()).await.unwrap();
    assert_eq!(first.graph.packages[0].name, "first");
    assert_eq!(graph.current().unwrap().id, first.id);

    std::fs::write(&manifest, r#"{"name":"second"}"#).unwrap();
    let second = graph.refresh(CancellationToken::new()).await.unwrap();
    assert_ne!(first.id, second.id);
    assert!(matches!(
        graph.get(&first.id),
        Err(DiscoveryError::SnapshotNotFound(_))
    ));
    assert_eq!(
        graph.get(&second.id).unwrap().graph.packages[0].name,
        "second"
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        graph.refresh(cancelled).await,
        Err(DiscoveryError::Cancelled)
    ));
    assert_eq!(graph.current().unwrap().id, second.id);

    let child = dir.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("package.json"), r#"{"name":"child"}"#).unwrap();
    let added = graph.refresh(CancellationToken::new()).await.unwrap();
    assert_ne!(second.id, added.id);
    assert!(added
        .graph
        .packages
        .iter()
        .any(|package| package.name == "child"));
    std::fs::remove_file(child.join("package.json")).unwrap();
    let removed = graph.refresh(CancellationToken::new()).await.unwrap();
    assert_ne!(added.id, removed.id);
    assert!(removed
        .graph
        .packages
        .iter()
        .all(|package| package.name != "child"));
}
