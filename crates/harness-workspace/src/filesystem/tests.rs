use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn fs_workspace_read_write_roundtrip() {
    let dir = tempdir().unwrap();
    let ws = FsWorkspace::new(dir.path().to_path_buf());

    ws.write("hello.txt", "world\n").await.unwrap();
    let contents = ws.read("hello.txt").await.unwrap();
    assert_eq!(contents, "world\n");
}

#[tokio::test]
async fn fs_workspace_blocks_path_traversal() {
    let dir = tempdir().unwrap();
    let ws = FsWorkspace::new(dir.path().to_path_buf());

    let result = ws.read("../../etc/passwd").await;
    assert!(matches!(result, Err(WorkspaceError::PathTraversal { .. })));
}

#[cfg(unix)]
#[tokio::test]
async fn fs_workspace_blocks_read_through_a_symlink_escaping_root() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "top secret")
        .await
        .unwrap();

    // A symlink *inside* the workspace root pointing to a directory
    // outside it. Lexically, "escape/secret.txt" starts with `root` and
    // would pass the old `resolve_path`-only check, but the OS follows
    // the symlink at actual open time and would read the outside file.
    tokio::fs::symlink(outside.path(), dir.path().join("escape"))
        .await
        .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = ws.read("escape/secret.txt").await;
    assert!(
            matches!(result, Err(WorkspaceError::PathTraversal { .. })),
            "reading through a symlink that escapes the workspace root must be rejected, got {result:?}"
        );
}

#[cfg(unix)]
#[tokio::test]
async fn fs_workspace_blocks_write_through_a_symlink_escaping_root() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();

    // The leaf itself is a symlink pointing outside root.
    tokio::fs::symlink(
        outside.path().join("clobbered.txt"),
        dir.path().join("escape.txt"),
    )
    .await
    .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = ws.write("escape.txt", "attacker-controlled content").await;
    assert!(
            matches!(result, Err(WorkspaceError::PathTraversal { .. })),
            "writing through a symlink that escapes the workspace root must be rejected, got {result:?}"
        );
    assert!(
        !outside.path().join("clobbered.txt").exists(),
        "the file outside the workspace root must never be created"
    );
}

#[tokio::test]
async fn fs_workspace_write_is_atomic_no_partial_file_on_crash() {
    let dir = tempdir().unwrap();
    let ws = FsWorkspace::new(dir.path().to_path_buf());

    // Write once so the target exists with known-good content.
    ws.write("data.txt", "original content").await.unwrap();

    // A real crash mid-write can't be simulated directly, but atomicity
    // is exactly the property that a temp-file-then-rename gives us: the
    // target path only ever shows the fully-old or fully-new content,
    // never a partial write. Confirm the write leaves no stray temp
    // file behind in the target directory once it completes.
    ws.write("data.txt", "replacement content").await.unwrap();
    assert_eq!(ws.read("data.txt").await.unwrap(), "replacement content");

    let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    assert_eq!(
        names,
        vec!["data.txt"],
        "no leftover temp file should remain after a successful atomic write"
    );
}

#[tokio::test]
async fn fs_workspace_read_caps_size_and_marks_truncation() {
    let dir = tempdir().unwrap();
    let oversized = "a".repeat(MAX_READ_BYTES as usize + 1024);
    fs::write(dir.path().join("huge.txt"), &oversized)
        .await
        .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let contents = ws.read("huge.txt").await.unwrap();

    assert!(
        contents.len()
            <= MAX_READ_BYTES as usize + "\n... (truncated, exceeds read size limit)".len(),
        "read result must not exceed the cap plus the truncation marker, got {} bytes",
        contents.len()
    );
    assert!(
        contents.ends_with("... (truncated, exceeds read size limit)"),
        "truncated read must carry an explicit marker"
    );
}

#[tokio::test]
async fn fs_workspace_read_under_cap_is_not_marked_truncated() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("small.txt"), "well under the cap")
        .await
        .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let contents = ws.read("small.txt").await.unwrap();
    assert_eq!(contents, "well under the cap");
}

#[tokio::test]
async fn fs_workspace_search_stops_at_the_match_cap_and_reports_truncation() {
    let dir = tempdir().unwrap();
    // One file with far more matching lines than MAX_SEARCH_MATCHES, so
    // traversal must stop mid-file rather than collecting them all.
    let content = "needle\n".repeat(MAX_SEARCH_MATCHES + 500);
    fs::write(dir.path().join("haystack.txt"), &content)
        .await
        .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = ws.search("needle").await.unwrap();

    assert!(
        result.matches.len() <= MAX_SEARCH_MATCHES,
        "search must stop collecting matches at the cap, got {}",
        result.matches.len()
    );
    assert!(
        result.truncated,
        "search must report that it stopped early rather than silently under-reporting"
    );
}

#[tokio::test]
async fn fs_workspace_search_under_cap_is_not_marked_truncated() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("small.txt"), "needle once")
        .await
        .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = ws.search("needle").await.unwrap();
    assert_eq!(result.total_count, 1);
    assert!(!result.truncated);
}

#[tokio::test]
async fn fs_workspace_search_finds_matches() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("greeting.rs"),
        "fn hello() { println!(\"hi\"); }",
    )
    .await
    .unwrap();

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = ws.search("println").await.unwrap();

    assert_eq!(result.total_count, 1);
    assert_eq!(result.matches[0].file_path, PathBuf::from("greeting.rs"));
    assert_eq!(result.matches[0].line_number, 1);
}

/// M3: a symlink pointing back at its own parent directory creates an
/// infinite directory tree if naively followed — `sub/link -> sub`
/// means `sub/link/link/link/...` never terminates. `search` must
/// finish promptly (bounded by the tokio test's own timeout) rather than
/// looping forever, and must still find the real match that exists
/// alongside the cycle.
#[tokio::test]
async fn fs_workspace_search_does_not_loop_on_a_symlink_cycle() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).await.unwrap();
    fs::write(sub.join("real.rs"), "needle here").await.unwrap();
    #[cfg(unix)]
    tokio::fs::symlink(&sub, sub.join("cycle")).await.unwrap();
    #[cfg(not(unix))]
    panic!("this test only runs on unix, where tokio::fs::symlink(dir) is supported");

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), ws.search("needle"))
        .await
        .expect("search must not loop forever on a symlink cycle")
        .expect("search must succeed despite the cycle");

    assert_eq!(result.total_count, 1);
    assert_eq!(result.matches[0].file_path, PathBuf::from("sub/real.rs"));
}

/// M3: same cycle hazard, for `list_files` — including with
/// `max_depth: 0` ("unlimited"), the one setting that gives the
/// depth-based bound no chance to help at all.
#[tokio::test]
async fn fs_workspace_list_files_does_not_loop_on_a_symlink_cycle() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).await.unwrap();
    fs::write(sub.join("real.rs"), "content").await.unwrap();
    #[cfg(unix)]
    tokio::fs::symlink(&sub, sub.join("cycle")).await.unwrap();
    #[cfg(not(unix))]
    panic!("this test only runs on unix, where tokio::fs::symlink(dir) is supported");

    let ws = FsWorkspace::new(dir.path().to_path_buf());
    let files = tokio::time::timeout(std::time::Duration::from_secs(5), ws.list_files(0))
        .await
        .expect("list_files must not loop forever on a symlink cycle, even with max_depth: 0")
        .expect("list_files must succeed despite the cycle");

    assert!(
        files.iter().any(|f| f.path.ends_with("real.rs")),
        "the real file alongside the cycle must still be listed: {files:?}"
    );
    assert!(
            files.iter().all(|f| f.path.file_name().and_then(|n| n.to_str()) != Some("cycle")),
            "the symlink itself must not be listed either, matching this workspace's no-symlinks policy: {files:?}"
        );
}
