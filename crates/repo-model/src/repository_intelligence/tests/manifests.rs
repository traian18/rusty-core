use super::*;
use crate::import_graph::ImportIndex;

#[test]
fn package_json_array_workspaces_are_supported() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"root","workspaces":["apps/*","tools/*"]}"#,
    )
    .unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let root = graph
        .packages
        .iter()
        .find(|package| package.name == "root")
        .unwrap();
    assert_eq!(
        root.facts
            .workspace_members
            .iter()
            .map(|member| member.as_path())
            .collect::<Vec<_>>(),
        vec![Path::new("apps/*"), Path::new("tools/*")]
    );
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("apps/*")));
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("tools/*")));
}

#[test]
fn malformed_cargo_manifest_is_a_diagnostic() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace\nmembers = ").unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(graph.diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("malformed Cargo.toml")
            && diagnostic
                .path
                .as_ref()
                .is_some_and(|path| path.as_path() == Path::new("Cargo.toml"))
    }));
    assert!(graph.packages.is_empty());
}

#[test]
fn main_rs_is_captured_as_a_source_root() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"binary\"\nversion=\"0.1\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        graph.packages[0]
            .facts
            .source_roots
            .iter()
            .map(|root| root.as_path())
            .collect::<Vec<_>>(),
        vec![Path::new("src/main.rs")]
    );
}

#[test]
fn unsupported_npm_workspaces_and_missing_manifest_reads_are_diagnostics() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"root","workspaces":{"unexpected":true}}"#,
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("broken")).unwrap();
    std::fs::write(dir.path().join("broken/package.json"), b"\xff\xfe").unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(graph.diagnostics.iter().any(|diagnostic| diagnostic
        .message
        .contains("unsupported package.json workspaces")));
    assert!(graph.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .path
            .as_ref()
            .is_some_and(|path| path.as_path() == Path::new("broken/package.json"))
    }));
    assert!(graph.packages.iter().any(|package| package.name == "root"));
}

#[test]
fn cargo_dependencies_and_snapshot_refresh_are_supported() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"first\"\nversion = \"0.1\"\n[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    let first = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(first.packages[0].facts.declared_dependencies, vec!["serde"]);
    let snapshots = InMemoryRepositorySnapshotRepository::new(1).unwrap();
    let old_id = snapshot_id(&first, dir.path()).unwrap();
    snapshots
        .publish(RepositorySnapshot {
            id: old_id.clone(),
            graph: first,
            imports: ImportIndex::default(),
            groups: Default::default(),
            frameworks: Default::default(),
        })
        .unwrap();
    std::fs::write(
        &manifest,
        "[package]\nname = \"second\"\nversion = \"0.1\"\n",
    )
    .unwrap();
    let current_graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let current_id = snapshot_id(&current_graph, dir.path()).unwrap();
    snapshots
        .publish(RepositorySnapshot {
            id: current_id.clone(),
            graph: current_graph,
            imports: ImportIndex::default(),
            groups: Default::default(),
            frameworks: Default::default(),
        })
        .unwrap();
    assert_ne!(old_id, current_id);
    assert!(snapshots.get(&old_id).is_err());
    assert!(snapshots.get(&current_id).is_ok());
}

/// A manifest format the crate knows nothing about: the first line is the package name.
struct LineManifestAdapter;

impl ManifestAdapter for LineManifestAdapter {
    fn id(&self) -> &'static str {
        "line-manifest"
    }

    fn file_name(&self) -> &'static str {
        "package.line"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let name = input.text.lines().next().unwrap_or("").trim();
        if name.is_empty() {
            return ManifestParse {
                diagnostics: vec![DiscoveryDiagnostic::for_path(input.path, "no name")],
                ..ManifestParse::default()
            };
        }
        ManifestParse {
            packages: vec![Package::declared(
                name.to_owned(),
                input.path.clone(),
                vec!["dep".into()],
            )],
            ..ManifestParse::default()
        }
    }
}

#[test]
fn manifest_adapters_are_replaceable_behind_the_contract() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.line"), "custom\n").unwrap();
    std::fs::create_dir(dir.path().join("empty")).unwrap();
    std::fs::write(dir.path().join("empty/package.line"), "\n").unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"ignored\"\nversion = \"0.1\"\n",
    )
    .unwrap();
    let discovery = FsRepositoryDiscovery::with_adapters(vec![Arc::new(LineManifestAdapter)]);
    let graph = discovery
        .scan(
            dir.path(),
            &DiscoveryLimits::default(),
            &CancellationToken::new(),
        )
        .unwrap();
    // Only the registered format is read; Cargo.toml is not this discovery's concern.
    assert_eq!(
        graph
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["custom"]
    );
    assert_eq!(graph.packages[0].facts.declared_dependencies, vec!["dep"]);
    assert!(graph.diagnostics.iter().any(|d| d.message == "no name"));
}

#[test]
fn dev_build_and_peer_dependencies_are_recorded_separately() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("app")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"rust-app\"\nversion = \"0.1\"\n[dependencies]\nserde = \"1\"\nboth = \"1\"\n[dev-dependencies]\ntempfile = \"3\"\nboth = \"1\"\n[build-dependencies]\ncc = \"1\"\n[target.'cfg(unix)'.dev-dependencies]\nlibc = \"0.2\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/package.json"),
        r#"{"name":"web","dependencies":{"react":"1"},"optionalDependencies":{"fsevents":"1"},"devDependencies":{"vitest":"1","react":"1"},"peerDependencies":{"typescript":"5"}}"#,
    )
    .unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let rust = graph
        .packages
        .iter()
        .find(|p| p.name == "rust-app")
        .unwrap();
    assert_eq!(
        rust.facts.declared_dependencies,
        vec!["both", "cc", "libc", "serde", "tempfile"]
    );
    assert_eq!(rust.facts.dev_dependencies, vec!["cc", "libc", "tempfile"]);
    let web = graph.packages.iter().find(|p| p.name == "web").unwrap();
    assert_eq!(web.facts.dev_dependencies, vec!["typescript", "vitest"]);
    assert!(web
        .facts
        .declared_dependencies
        .contains(&"vitest".to_owned()));
}
