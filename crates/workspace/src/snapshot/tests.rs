use tempfile::tempdir;

use super::SnapshotWorkspace;
use crate::{filesystem::FsWorkspace, workspace::Workspace};

#[tokio::test]
async fn writes_to_the_snapshot_never_reach_the_source() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("data.txt"), "original")
        .await
        .unwrap();
    let snapshot = SnapshotWorkspace::create(dir.path()).await.unwrap();
    let source = FsWorkspace::new(dir.path().to_path_buf());

    snapshot.write("data.txt", "child edit").await.unwrap();

    assert_eq!(snapshot.read("data.txt").await.unwrap(), "child edit");
    assert_eq!(source.read("data.txt").await.unwrap(), "original");
}
