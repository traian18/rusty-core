use std::sync::Arc;

use tempfile::tempdir;

use super::ReadOnlyWorkspace;
use crate::{error::WorkspaceError, filesystem::FsWorkspace, workspace::Workspace};

#[tokio::test]
async fn reads_pass_through() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("data.txt"), "secret")
        .await
        .unwrap();
    let read_only = ReadOnlyWorkspace::new(Arc::new(FsWorkspace::new(dir.path().to_path_buf())));

    assert_eq!(read_only.read("data.txt").await.unwrap(), "secret");
}

#[tokio::test]
async fn writes_are_refused() {
    let dir = tempdir().unwrap();
    let read_only = ReadOnlyWorkspace::new(Arc::new(FsWorkspace::new(dir.path().to_path_buf())));

    let result = read_only.write("data.txt", "new content").await;

    assert!(matches!(result, Err(WorkspaceError::Isolated)));
}
