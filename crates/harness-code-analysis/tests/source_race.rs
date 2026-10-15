//! Adversarial replacement races against the Unix handle-based source scanner.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_code_analysis::{AnalysisSource, FileSystemSource, SourceDiscovery};
use tokio_util::sync::CancellationToken;

mod support;
use support::{temp_root, write_files};

async fn discover_with_hook(
    root: &Path,
    hook: impl Fn(&str) + Send + Sync + 'static,
) -> SourceDiscovery {
    let source = FileSystemSource::new(1 << 20, 8).with_open_hook(Arc::new(hook));
    source
        .discover(root, 100, &CancellationToken::new())
        .await
        .unwrap()
}

fn outside(name: &str) -> (PathBuf, PathBuf) {
    let dir = temp_root(name);
    let secret = dir.join("secret.rs");
    fs::write(&secret, "fn stolen() {}\n").unwrap();
    (dir, secret)
}

fn assert_not_leaked(discovery: &SourceDiscovery) {
    for file in &discovery.files {
        assert!(
            !String::from_utf8_lossy(&file.bytes).contains("stolen"),
            "{} leaked content from outside the root",
            file.path
        );
    }
}

#[tokio::test]
async fn file_swapped_for_symlink_after_listing_is_not_followed() {
    let root = temp_root("race-file");
    write_files(&root, &[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let (outside_dir, secret) = outside("race-file-outside");
    let target = root.join("b.rs");
    let secret_for_hook = secret.clone();
    let discovery = discover_with_hook(&root, move |path| {
        if path == "b.rs" {
            fs::remove_file(&target).unwrap();
            symlink(&secret_for_hook, &target).unwrap();
        }
    })
    .await;
    assert_not_leaked(&discovery);
    assert!(discovery.files.iter().any(|f| f.path == "a.rs"));
    assert!(!discovery.files.iter().any(|f| f.path == "b.rs"));
    assert!(
        discovery
            .diagnostics
            .iter()
            .any(|d| d.path.as_deref() == Some("b.rs")),
        "the rejected file is reported: {:?}",
        discovery.diagnostics
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside_dir);
}

#[tokio::test]
async fn directory_swapped_for_symlink_after_listing_is_not_followed() {
    let root = temp_root("race-dir");
    write_files(
        &root,
        &[("a/one.rs", "fn one() {}\n"), ("z/two.rs", "fn two() {}\n")],
    );
    let (outside_dir, _) = outside("race-dir-outside");
    let victim = root.join("z");
    let outside_for_hook = outside_dir.clone();
    // `a` sorts before `z`, so the swap happens while `z` was already listed but not opened.
    let discovery = discover_with_hook(&root, move |path| {
        if path == "a/one.rs" {
            fs::remove_dir_all(&victim).unwrap();
            symlink(&outside_for_hook, &victim).unwrap();
        }
    })
    .await;
    assert_not_leaked(&discovery);
    assert!(discovery.files.iter().any(|f| f.path == "a/one.rs"));
    assert!(!discovery.files.iter().any(|f| f.path.starts_with("z/")));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside_dir);
}

#[tokio::test]
async fn file_replaced_by_another_file_after_listing_is_skipped() {
    let root = temp_root("race-replace");
    write_files(&root, &[("a.rs", "fn original() {}\n")]);
    let path = root.join("a.rs");
    let discovery = discover_with_hook(&root, move |relative| {
        if relative == "a.rs" {
            let replacement = path.with_extension("tmp");
            fs::write(&replacement, "fn swapped() {}\n").unwrap();
            fs::rename(&replacement, &path).unwrap();
        }
    })
    .await;
    assert!(
        discovery.files.is_empty(),
        "identity change must be detected"
    );
    assert!(discovery
        .diagnostics
        .iter()
        .any(|d| d.message.contains("changed while")));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn file_replaced_by_fifo_does_not_block_or_leak() {
    let root = temp_root("race-fifo");
    write_files(&root, &[("a.rs", "fn a() {}\n")]);
    let path = root.join("a.rs");
    let discovery = discover_with_hook(&root, move |relative| {
        if relative == "a.rs" {
            fs::remove_file(&path).unwrap();
            let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        }
    })
    .await;
    assert!(discovery.files.is_empty());
    assert!(!discovery.diagnostics.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn root_symlink_target_is_the_authorised_root_but_nested_links_are_not() {
    let (outside_dir, _) = outside("race-root-outside");
    let real = temp_root("race-root-real");
    write_files(&real, &[("inside.rs", "fn inside() {}\n")]);
    symlink(&outside_dir, real.join("escape")).unwrap();
    let discovery = FileSystemSource::new(1 << 20, 8)
        .discover(&real, 100, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        discovery
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["inside.rs"]
    );
    assert!(discovery
        .diagnostics
        .iter()
        .any(|d| d.path.as_deref() == Some("escape") && d.message.contains("symbolic link")));
    assert_not_leaked(&discovery);
    let _ = fs::remove_dir_all(real);
    let _ = fs::remove_dir_all(outside_dir);
}
