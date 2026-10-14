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
    let files = source
        .discover(&root, 2, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["nested/a.rs", "z.rs"]
    );
    assert!(files.iter().all(|file| !file.hash.is_empty()));
    assert!(matches!(
        source.discover(&root, 1, &CancellationToken::new()).await,
        Err(CallGraphError::LimitExceeded(_))
    ));
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
    let files = FileSystemSource::new(64, 4)
        .discover(&root, 10, &CancellationToken::new())
        .await
        .unwrap();
    assert!(files.is_empty());
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}
