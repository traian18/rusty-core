use super::*;

#[tokio::test]
async fn discovery_is_substitutable_and_ordered() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"root"}"#).unwrap();
    std::fs::create_dir(dir.path().join("child")).unwrap();
    std::fs::write(dir.path().join("child/package.json"), r#"{"name":"child"}"#).unwrap();
    let graph = FsRepositoryDiscovery::default()
        .discover(
            dir.path(),
            &DiscoveryLimits::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        graph
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["child", "root"]
    );
    assert_eq!(
        Substitute
            .discover(
                dir.path(),
                &graph.effective_limits,
                CancellationToken::new()
            )
            .await
            .unwrap()
            .id
            .0,
        "substitute"
    );
}

#[test]
fn multi_package_cargo_workspace_is_discovered() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/one\", \"crates/two\"]\n",
    )
    .unwrap();
    for member in ["one", "two"] {
        let member_dir = dir.path().join("crates").join(member);
        std::fs::create_dir_all(member_dir.join("src")).unwrap();
        std::fs::write(
            member_dir.join("Cargo.toml"),
            format!("[package]\nname=\"{member}\"\nversion=\"0.1\"\n"),
        )
        .unwrap();
        std::fs::write(member_dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    }
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let mut names = graph
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, vec!["one", "two"]);
    for package in &graph.packages {
        assert_eq!(
            package
                .facts
                .workspace_members
                .iter()
                .map(|member| member.as_path().to_path_buf())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("crates/one"), PathBuf::from("crates/two")]
        );
    }
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/one")));
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/two")));
}

#[test]
fn nested_workspace_roots_do_not_inherit_outer_members() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/a\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("crates/a/b")).unwrap();
    std::fs::write(
        dir.path().join("crates/a/Cargo.toml"),
        "[package]\nname=\"a\"\nversion=\"0.1\"\n[workspace]\nmembers = [\"b\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("crates/a/b/Cargo.toml"),
        "[package]\nname=\"b\"\nversion=\"0.1\"\n",
    )
    .unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let members_of = |name: &str| {
        graph
            .packages
            .iter()
            .find(|package| package.name == name)
            .unwrap()
            .facts
            .workspace_members
            .iter()
            .map(|member| member.as_path().to_path_buf())
            .collect::<Vec<_>>()
    };
    assert_eq!(members_of("a"), vec![PathBuf::from("crates/a/b")]);
    assert_eq!(members_of("b"), vec![PathBuf::from("crates/a/b")]);
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/a")));
    assert!(graph
        .workspace_members
        .iter()
        .any(|member| member.as_path() == Path::new("crates/a/b")));
}

#[test]
fn diagnostics_cover_malformed_duplicate_and_limit_cases() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{").unwrap();
    for folder in ["a", "b"] {
        std::fs::create_dir(dir.path().join(folder)).unwrap();
        std::fs::write(
            dir.path().join(folder).join("package.json"),
            r#"{"name":"duplicate"}"#,
        )
        .unwrap();
    }
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("malformed")));
    assert!(graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("duplicate")));
    let limited = discover_filesystem(
        dir.path(),
        &DiscoveryLimits {
            max_entries: 1,
            max_file_bytes: 2,
            max_depth: 0,
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(limited.omitted_entries > 0);
    assert_eq!(limited.effective_limits.max_entries, 1);
    for folder in ["target", "node_modules", ".git"] {
        let ignored = dir.path().join(folder);
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(
            ignored.join("package.json"),
            format!(r#"{{"name":"generated-{folder}"}}"#),
        )
        .unwrap();
    }
    let after_ignored = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(after_ignored
        .packages
        .iter()
        .all(|p| !p.name.starts_with("generated-")));
    assert_eq!(
        RootRelativePath::new("./a/b")
            .unwrap()
            .as_path()
            .components()
            .count(),
        2
    );
    assert!(RootRelativePath::new("a/../b").is_err());
}

#[test]
fn cancelled_scans_and_symlink_manifests_are_safe() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"root"}"#).unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        discover_filesystem(dir.path(), &DiscoveryLimits::default(), &cancelled),
        Err(DiscoveryError::Cancelled)
    ));

    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("package.json"), r#"{"name":"outside"}"#).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("package.json"),
            dir.path().join("linked.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked-dir")).unwrap();
        let graph = discover_filesystem(
            dir.path(),
            &DiscoveryLimits::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(graph
            .packages
            .iter()
            .all(|package| package.name != "outside"));
    }
}

#[test]
fn cancellation_is_observed_at_entry_processing_boundaries() {
    let dir = tempdir().unwrap();
    for index in 0..128 {
        std::fs::create_dir(dir.path().join(format!("folder-{index:03}"))).unwrap();
    }
    let token = CancellationToken::new();
    let cloned = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1));
        cloned.cancel();
    });
    let result = discover_filesystem(dir.path(), &DiscoveryLimits::default(), &token);
    assert!(matches!(result, Err(DiscoveryError::Cancelled)));
}

#[cfg(unix)]
#[test]
fn symlinked_directory_is_not_traversed() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("package.json"), r#"{"name":"outside"}"#).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
    let graph = discover_filesystem(
        dir.path(),
        &DiscoveryLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(graph.packages.is_empty());
}

/// A manifest format named by pattern (like `*.csproj`), to prove discovery asks adapters by name.
struct PatternManifest;

impl ManifestAdapter for PatternManifest {
    fn id(&self) -> &'static str {
        "pattern"
    }
    fn file_name(&self) -> &'static str {
        "*.proj"
    }
    fn handles(&self, file_name: &str) -> bool {
        file_name.ends_with(".proj")
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let name = input
            .path
            .as_path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unnamed")
            .to_owned();
        ManifestParse {
            packages: vec![Package::declared(name, input.path.clone(), vec![])],
            ..ManifestParse::default()
        }
    }
}

#[test]
fn manifests_named_by_pattern_and_extra_source_extensions_are_discovered() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/App.proj"), "x").unwrap();
    std::fs::write(dir.path().join("app/main.py"), "import os\n").unwrap();
    std::fs::write(dir.path().join("app/lib.rs"), "").unwrap();
    let limits = DiscoveryLimits::default();
    let cancel = CancellationToken::new();

    let defaults = FsRepositoryDiscovery::with_adapters(vec![Arc::new(PatternManifest)])
        .scan(dir.path(), &limits, &cancel)
        .unwrap();
    assert_eq!(
        defaults
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["App"]
    );
    assert!(
        defaults
            .source_files
            .iter()
            .all(|f| f.as_path().extension().unwrap() == "rs"),
        "Python files are not indexed unless a language registers `py`"
    );

    let widened = FsRepositoryDiscovery::with_adapters(vec![Arc::new(PatternManifest)])
        .with_source_extensions(vec!["rs".into(), "py".into()])
        .scan(dir.path(), &limits, &cancel)
        .unwrap();
    let names = widened
        .source_files
        .iter()
        .map(|f| f.as_path().display().to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, ["app/lib.rs", "app/main.py"]);
}

#[test]
fn the_builder_passes_source_extensions_to_discovery() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"p"}"#).unwrap();
    std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let service = RepositoryGraphBuilder::new(root)
        .unwrap()
        .source_extensions(vec!["go".into()])
        .build();
    let snapshot = runtime
        .block_on(service.refresh(CancellationToken::new()))
        .unwrap();
    assert_eq!(snapshot.graph.source_files.len(), 1);
}
