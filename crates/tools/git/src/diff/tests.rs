use super::*;

fn init_repo_with_commit(dir: &Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).expect("init repo");
    std::fs::write(dir.join("a.txt"), "hello\n").expect("write file");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("a.txt")).expect("add path");
    index.write().expect("write index");
    let tree_id = index.write_tree().expect("write tree");
    let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
    {
        let tree = repo.find_tree(tree_id).expect("find tree");
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .expect("commit");
    }
    repo
}

#[tokio::test]
async fn shows_unstaged_working_tree_diff() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo_with_commit(dir.path());
    std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").expect("modify file");

    let tool = GitDiffTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(
            ToolInput {
                arguments: json!({ "staged": false }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute should succeed");

    assert!(!result.is_error);
    let diff = result.output["diff"].as_str().expect("diff string");
    assert!(diff.contains("+world"));
}

#[tokio::test]
async fn shows_staged_diff_against_head() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = init_repo_with_commit(dir.path());
    std::fs::write(dir.path().join("a.txt"), "staged change\n").expect("modify file");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("a.txt")).expect("add path");
    index.write().expect("write index");

    let tool = GitDiffTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(
            ToolInput {
                arguments: json!({ "staged": true }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute should succeed");

    assert!(!result.is_error);
    let diff = result.output["diff"].as_str().expect("diff string");
    assert!(diff.contains("staged change"));
}

fn commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .expect("add all");
    index.write().expect("write index");
    let tree = repo
        .find_tree(index.write_tree().expect("write tree"))
        .expect("find tree");
    let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .expect("commit");
}

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

/// a.txt, b.rs, c.rs each with 60 lines; the working tree then changes
/// lines 2 and 50 of every file (two separate hunks per file), and
/// line 50 of b.rs mentions `needle`.
fn repo_with_two_hunks_per_file(dir: &Path) {
    let repo = git2::Repository::init(dir).expect("init repo");
    for name in ["a.txt", "b.rs", "c.rs"] {
        std::fs::write(dir.join(name), numbered(60)).expect("write file");
    }
    commit_all(&repo, "initial");
    for name in ["a.txt", "b.rs", "c.rs"] {
        let tail = if name == "b.rs" {
            "needle here"
        } else {
            "changed"
        };
        let content = numbered(60)
            .replace("line 2\n", "line two\n")
            .replace("line 50\n", &format!("line 50 {tail}\n"));
        std::fs::write(dir.join(name), content).expect("modify file");
    }
}

async fn diff_with(dir: &Path, arguments: serde_json::Value) -> serde_json::Value {
    let result = GitDiffTool::new(dir.to_path_buf())
        .execute(ToolInput { arguments }, CancellationToken::new())
        .await
        .expect("execute should succeed");
    assert!(!result.is_error, "unexpected error: {}", result.output);
    result.output
}

fn file_paths(output: &serde_json::Value) -> Vec<String> {
    output["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn filters_by_pathspecs_and_summarizes_every_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_with_two_hunks_per_file(dir.path());

    let output = diff_with(dir.path(), json!({ "paths": ["*.rs"] })).await;

    assert_eq!(file_paths(&output), vec!["b.rs", "c.rs"]);
    assert_eq!(output["total_files"], 2);
    assert_eq!(output["files"][0]["additions"], 2);
    assert_eq!(output["files"][0]["deletions"], 2);
    assert_eq!(output["files"][0]["status"], "modified");
    assert_eq!(output["files"][0]["patch"], "full");
    assert!(!output["diff"].as_str().unwrap().contains("a.txt"));
    assert!(output.get("limits").is_none());
}

#[tokio::test]
async fn summary_only_returns_counts_without_patch_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_with_two_hunks_per_file(dir.path());

    let output = diff_with(dir.path(), json!({ "summary_only": true })).await;

    assert_eq!(output["diff"], "");
    assert_eq!(file_paths(&output), vec!["a.txt", "b.rs", "c.rs"]);
    assert!(output["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["patch"] == "summary"));
}

#[tokio::test]
async fn max_files_omits_later_patches_but_still_lists_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_with_two_hunks_per_file(dir.path());

    let output = diff_with(dir.path(), json!({ "max_files": 1 })).await;

    let states: Vec<_> = output["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["patch"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["full", "omitted", "omitted"]);
    assert_eq!(output["limits"]["omitted_files"], 2);
    assert!(output["diff"]
        .as_str()
        .unwrap()
        .contains("diff --git a/a.txt b/a.txt"));
    assert!(!output["diff"].as_str().unwrap().contains("b.rs"));
}

#[tokio::test]
async fn hunk_contains_keeps_only_matching_hunks_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_with_two_hunks_per_file(dir.path());

    let output = diff_with(dir.path(), json!({ "hunk_contains": "needle" })).await;
    let diff = output["diff"].as_str().unwrap();

    assert!(diff.contains("+line 50 needle here"));
    assert!(
        !diff.contains("line two"),
        "the non-matching hunk must be dropped"
    );
    let states: Vec<_> = output["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["patch"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["filtered_out", "full", "filtered_out"]);
    assert_eq!(output["files"][1]["hunks"], 2);
    assert_eq!(output["files"][1]["hunks_shown"], 1);
}

#[tokio::test]
async fn max_hunks_per_file_reports_what_it_left_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_with_two_hunks_per_file(dir.path());

    let output = diff_with(
        dir.path(),
        json!({ "paths": ["a.txt"], "max_hunks_per_file": 1 }),
    )
    .await;
    let diff = output["diff"].as_str().unwrap();

    assert!(diff.contains("+line two"));
    assert!(!diff.contains("line 50 changed"));
    assert!(diff.contains("... (1 more matching hunk(s) omitted)"));
    assert_eq!(output["limits"]["omitted_hunks"], 1);
}

#[tokio::test]
async fn max_bytes_truncates_on_a_character_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("init repo");
    std::fs::write(dir.path().join("euro.txt"), "start\n").expect("write");
    commit_all(&repo, "initial");
    // Multi-byte characters everywhere, so almost any byte cut lands mid-character.
    std::fs::write(
        dir.path().join("euro.txt"),
        "€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€\n".repeat(500),
    )
    .expect("modify");

    for max_bytes in [997, 998, 999, 1_000] {
        let output = diff_with(dir.path(), json!({ "max_bytes": max_bytes })).await;
        let diff = output["diff"].as_str().unwrap();
        assert!(diff.len() <= max_bytes, "{} > {max_bytes}", diff.len());
        assert!(diff.ends_with("... (diff truncated)"));
        assert_eq!(output["files"][0]["patch"], "partial");
        assert_eq!(output["limits"]["max_bytes"], max_bytes);
    }
}

#[test]
fn schema_exposes_the_filters_alongside_the_original_fields() {
    let schema = GitDiffTool::new(PathBuf::from("."))
        .descriptor()
        .input_schema;
    let properties = schema["properties"].as_object().expect("properties");
    for field in [
        "path",
        "staged",
        "paths",
        "hunk_contains",
        "max_hunks_per_file",
        "max_files",
        "max_bytes",
        "summary_only",
    ] {
        assert!(properties.contains_key(field), "missing {field}");
    }
}
