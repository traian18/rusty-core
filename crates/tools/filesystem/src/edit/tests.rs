use super::*;
use harness_workspace::FsWorkspace;
use tempfile::tempdir;

fn tool_for(root: std::path::PathBuf) -> EditTool {
    EditTool::new(Arc::new(FsWorkspace::new(root)))
}

#[tokio::test]
async fn whole_file_mode_still_works_unchanged() {
    let dir = tempdir().unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({ "path": "a.txt", "content": "hello world" }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(!result.is_error);
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("a.txt"))
            .await
            .unwrap(),
        "hello world"
    );
}

#[tokio::test]
async fn find_replace_unique_match_replaces() {
    let dir = tempdir().unwrap();
    tokio::fs::write(
        dir.path().join("a.txt"),
        "fn main() {\n    old_call();\n}\n",
    )
    .await
    .unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({
                    "path": "a.txt",
                    "old_text": "old_call();",
                    "new_text": "new_call();"
                }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(
        !result.is_error,
        "expected success, got {:?}",
        result.output
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("a.txt"))
            .await
            .unwrap(),
        "fn main() {\n    new_call();\n}\n"
    );
}

#[tokio::test]
async fn find_replace_missing_match_errors_without_writing() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("a.txt"), "unchanged content")
        .await
        .unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({
                    "path": "a.txt",
                    "old_text": "does not appear",
                    "new_text": "replacement"
                }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(result.is_error);
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("a.txt"))
            .await
            .unwrap(),
        "unchanged content",
        "the file must be left untouched when old_text is not found"
    );
}

#[tokio::test]
async fn find_replace_ambiguous_match_errors_without_writing() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("a.txt"), "dup\ndup\n")
        .await
        .unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({
                    "path": "a.txt",
                    "old_text": "dup",
                    "new_text": "unique"
                }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(result.is_error);
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("a.txt"))
            .await
            .unwrap(),
        "dup\ndup\n",
        "the file must be left untouched when old_text is ambiguous"
    );
}

#[tokio::test]
async fn specifying_both_content_and_old_text_is_rejected() {
    let dir = tempdir().unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({
                    "path": "a.txt",
                    "content": "whole file",
                    "old_text": "x",
                    "new_text": "y"
                }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(result.is_error);
}

#[tokio::test]
async fn specifying_neither_mode_is_rejected() {
    let dir = tempdir().unwrap();
    let tool = tool_for(dir.path().to_path_buf());

    let result = tool
        .execute(
            ToolInput {
                arguments: json!({ "path": "a.txt" }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(result.is_error);
}
