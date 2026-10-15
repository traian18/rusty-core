use harness_code_analysis::{AnalysisSource, CallGraphError, FileSystemSource};
use tokio_util::sync::CancellationToken;

fn root() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("call-graph-source-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn discovery_is_deterministic_root_relative_and_bounded() {
    let root = root();
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("z.rs"), b"fn z() {}\n").unwrap();
    std::fs::write(root.join("nested/a.rs"), b"fn a() {}\n").unwrap();
    let source = FileSystemSource::new(64, 4);
    let discovery = source
        .discover(&root, 2, &CancellationToken::new())
        .await
        .unwrap();
    let files = discovery.files;
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["nested/a.rs", "z.rs"]
    );
    let limited = source
        .discover(&root, 1, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(limited.files.len(), 1);
    assert!(limited
        .diagnostics
        .iter()
        .any(|item| item.message.contains("file count limit")));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn discovery_rejects_invalid_utf8_and_enforces_exact_byte_boundary() {
    let root = root();
    std::fs::write(root.join("valid.rs"), b"1234").unwrap();
    let source = FileSystemSource::new(4, 2);
    assert_eq!(
        source
            .discover(&root, 1, &CancellationToken::new())
            .await
            .unwrap()
            .files[0]
            .bytes,
        b"1234"
    );

    std::fs::write(root.join("valid.rs"), b"12345").unwrap();
    let oversized = source
        .discover(&root, 1, &CancellationToken::new())
        .await
        .unwrap();
    assert!(oversized.files.is_empty());
    assert!(oversized
        .diagnostics
        .iter()
        .any(|item| item.message.contains("file size limit")));

    std::fs::write(root.join("valid.rs"), [0xff, 0xfe]).unwrap();
    let bytes = source
        .discover(&root, 1, &CancellationToken::new())
        .await
        .unwrap()
        .files[0]
        .bytes
        .clone();
    assert_eq!(bytes, [0xff, 0xfe]);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn discovery_reports_unreadable_source_as_failure() {
    let root = root();
    std::fs::write(root.join("locked.rs"), b"fn locked() {}\n").unwrap();
    let source = FileSystemSource::new(64, 4);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("locked.rs"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        if unsafe { libc::geteuid() } != 0 {
            let result = source
                .discover(&root, 1, &CancellationToken::new())
                .await
                .unwrap();
            assert!(result.files.is_empty());
            assert!(result
                .diagnostics
                .iter()
                .any(|item| item.path.as_deref() == Some("locked.rs")));
        }
        std::fs::set_permissions(
            root.join("locked.rs"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    #[cfg(not(unix))]
    let _ = source;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn discovery_reports_depth_and_aggregate_byte_omissions() {
    let root = root();
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
    std::fs::write(root.join("a.rs"), b"1234567890").unwrap();
    std::fs::write(root.join("b.rs"), b"abcdefghij").unwrap();
    std::fs::write(root.join("nested/deeper/c.rs"), b"1234").unwrap();
    let depth_limited = FileSystemSource::new(64, 1)
        .discover(&root, 10, &CancellationToken::new())
        .await
        .unwrap();
    assert!(depth_limited
        .diagnostics
        .iter()
        .any(|item| item.message.contains("depth limit")));

    let byte_limited = FileSystemSource::new(10, 4)
        .with_total_byte_limit(12)
        .discover(&root, 10, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(byte_limited.files.len(), 1);
    assert!(byte_limited
        .diagnostics
        .iter()
        .any(|item| item.message.contains("aggregate source byte limit")));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn discovery_rejects_relative_root_and_cancelled_work() {
    let source = FileSystemSource::new(64, 4);
    assert!(matches!(
        source
            .discover(
                std::path::Path::new("relative"),
                10,
                &CancellationToken::new()
            )
            .await,
        Err(CallGraphError::InvalidRequest(_))
    ));
    let root = root();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        source.discover(&root, 10, &cancel).await,
        Err(CallGraphError::Cancelled)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn discovery_skips_symlink_files_and_directories() {
    use std::os::unix::fs::symlink;
    let root = root();
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.rs"), b"fn secret() {}\n").unwrap();
    symlink(outside.join("secret.rs"), root.join("linked.rs")).unwrap();
    symlink(&outside, root.join("linked-dir")).unwrap();
    let discovery = FileSystemSource::new(64, 4)
        .discover(&root, 10, &CancellationToken::new())
        .await
        .unwrap();
    assert!(discovery.files.is_empty());
    assert!(discovery
        .diagnostics
        .iter()
        .any(|item| item.message == "symbolic link skipped"));
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}
