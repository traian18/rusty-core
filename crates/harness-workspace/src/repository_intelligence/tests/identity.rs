use super::*;

#[test]
fn snapshot_identity_includes_diagnostic_and_omitted_work_multiplicity() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates/member/src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/member\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("crates/member/Cargo.toml"),
        "[package]\nname=\"member\"\nversion=\"0.1\"\n[target.'cfg(unix)'.dependencies]\nlibc=\"0.2\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("crates/member/src/lib.rs"),
        "pub fn hello() {}\n",
    )
    .unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/member")));
    assert!(graph.packages[0]
        .facts
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/member")));
    assert!(graph.packages[0]
        .facts
        .source_roots
        .iter()
        .any(|root| root.as_path() == Path::new("crates/member/src/lib.rs")));
    std::fs::create_dir_all(dir.path().join("crates/member-extra")).unwrap();
    std::fs::write(
        dir.path().join("crates/member-extra/Cargo.toml"),
        "[package]\nname=\"member-extra\"\nversion=\"0.1\"\n",
    )
    .unwrap();
    let prefix_case = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let sibling = prefix_case
        .packages
        .iter()
        .find(|package| package.name == "member-extra")
        .unwrap();
    assert!(!sibling
        .facts
        .workspace_members
        .contains(&RootRelativePath::new("crates/member").unwrap()));
    assert!(graph
        .packages
        .iter()
        .any(|package| package.name == "member"));
    assert!(!graph
        .packages
        .iter()
        .any(|package| package.name.starts_with("workspace:")));
    assert!(graph.packages[0]
        .facts
        .declared_dependencies
        .contains(&"libc".to_owned()));

    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"root","workspaces":{"packages":["apps/*"]}}"#,
    )
    .unwrap();
    let npm_graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(npm_graph
        .packages
        .iter()
        .find(|package| package.name == "root")
        .unwrap()
        .facts
        .workspace_members
        .iter()
        .any(|path| path.as_path() == Path::new("apps/*")));
    let id = snapshot_id(&npm_graph, dir.path()).unwrap();
    let same = snapshot_id(&npm_graph, dir.path()).unwrap();
    assert_eq!(id, same);
    std::fs::write(
        dir.path().join("crates/member/src/lib.rs"),
        "pub fn changed() {}\n",
    )
    .unwrap();
    let changed_source = snapshot_id(&npm_graph, dir.path()).unwrap();
    assert_ne!(id, changed_source);
    let changed_limits = RepositoryStructure {
        effective_limits: DiscoveryLimits {
            max_entries: 99,
            ..npm_graph.effective_limits.clone()
        },
        ..npm_graph.clone()
    };
    assert_ne!(id, snapshot_id(&changed_limits, dir.path()).unwrap());
    let changed_fact = RepositoryStructure {
        packages: npm_graph
            .packages
            .iter()
            .cloned()
            .map(|mut package| {
                package.facts.inferred_facts.push("adapter-fact".into());
                package
            })
            .collect(),
        ..npm_graph.clone()
    };
    assert_ne!(id, snapshot_id(&changed_fact, dir.path()).unwrap());
    let changed_diagnostic = RepositoryStructure {
        diagnostics: vec![DiscoveryDiagnostic {
            path: None,
            message: "new diagnostic".into(),
        }],
        ..npm_graph.clone()
    };
    assert_ne!(id, snapshot_id(&changed_diagnostic, dir.path()).unwrap());
    let changed_omitted = RepositoryStructure {
        omitted_entries: npm_graph.omitted_entries + 1,
        ..npm_graph.clone()
    };
    assert_ne!(id, snapshot_id(&changed_omitted, dir.path()).unwrap());
    let repeated_fact = RepositoryStructure {
        packages: npm_graph
            .packages
            .iter()
            .cloned()
            .map(|mut package| {
                package.facts.inferred_facts.push("adapter-fact".into());
                package.facts.inferred_facts.push("adapter-fact".into());
                package
            })
            .collect(),
        ..npm_graph.clone()
    };
    let repeated_id = snapshot_id(&repeated_fact, dir.path()).unwrap();
    let single_fact = RepositoryStructure {
        packages: npm_graph
            .packages
            .iter()
            .cloned()
            .map(|mut package| {
                package.facts.inferred_facts.push("adapter-fact".into());
                package
            })
            .collect(),
        ..npm_graph.clone()
    };
    assert_ne!(
        repeated_id,
        snapshot_id(&single_fact, dir.path()).unwrap(),
        "fact multiplicity contributes to identity"
    );
}
