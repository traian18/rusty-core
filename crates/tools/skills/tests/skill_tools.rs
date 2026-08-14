//! End-to-end behavior of `skill.load` and `skill.read` against a real
//! skill directory on disk.

use std::path::Path;
use std::sync::Arc;

use harness_skills::{SkillCatalog, SkillsConfig};
use harness_tool_skills::{SkillLoadTool, SkillReadTool};
use harness_tools::{CancellationToken, ToolExecutor, ToolInput};
use serde_json::json;
use tempfile::TempDir;

async fn write_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    tokio::fs::create_dir_all(&dir).await.expect("create dir");
    tokio::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: Does a thing.\nallowed-tools: [fs.read]\n---\n{body}"
        ),
    )
    .await
    .expect("write SKILL.md");
}

async fn catalog_from(root: &Path) -> Arc<SkillCatalog> {
    let config = SkillsConfig {
        workspace_root: None,
        include_user_dir: false,
        extra_roots: vec![root.to_path_buf()],
    };
    let (catalog, errors) = SkillCatalog::discover(&config).await;
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(catalog)
}

fn input(value: serde_json::Value) -> ToolInput {
    ToolInput { arguments: value }
}

#[tokio::test]
async fn load_returns_instructions_and_bundled_files() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report", "Render the template.\n").await;
    tokio::fs::write(temp.path().join("pdf-report").join("template.tex"), "\\doc")
        .await
        .expect("write");

    let tool = SkillLoadTool::new(catalog_from(temp.path()).await);
    let result = tool
        .execute(
            input(json!({ "name": "pdf-report" })),
            CancellationToken::new(),
        )
        .await
        .expect("execute");

    assert!(!result.is_error);
    assert_eq!(result.output["name"], "pdf-report");
    assert_eq!(result.output["instructions"], "Render the template.\n");
    assert_eq!(result.output["files"], json!(["template.tex"]));
    assert_eq!(result.output["allowed_tools"], json!(["fs.read"]));
}

/// A bad argument is a logical error the model can recover from, not an
/// infrastructure fault — so it must be `Ok(is_error: true)`, never `Err`.
#[tokio::test]
async fn load_reports_an_unknown_skill_as_a_logical_error() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "known", "Body.\n").await;

    let tool = SkillLoadTool::new(catalog_from(temp.path()).await);
    let result = tool
        .execute(input(json!({ "name": "absent" })), CancellationToken::new())
        .await
        .expect("execute must not return Err for a bad name");

    assert!(result.is_error);
    let message = result.output["error"].as_str().expect("error message");
    assert!(message.contains("absent"), "{message}");
    // Listing what is available is what turns a dead end into a retry.
    assert!(message.contains("known"), "{message}");
}

#[tokio::test]
async fn read_returns_a_bundled_file() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report", "Body.\n").await;
    tokio::fs::write(
        temp.path().join("pdf-report").join("template.tex"),
        "\\documentclass{article}",
    )
    .await
    .expect("write");

    let tool = SkillReadTool::new(catalog_from(temp.path()).await);
    let result = tool
        .execute(
            input(json!({ "skill": "pdf-report", "path": "template.tex" })),
            CancellationToken::new(),
        )
        .await
        .expect("execute");

    assert!(!result.is_error);
    assert_eq!(result.output["content"], "\\documentclass{article}");
}

#[tokio::test]
async fn read_refuses_to_escape_the_skill_directory() {
    let temp = TempDir::new().expect("tempdir");
    tokio::fs::write(temp.path().join("secret.txt"), "classified")
        .await
        .expect("write");
    write_skill(temp.path(), "pdf-report", "Body.\n").await;

    let tool = SkillReadTool::new(catalog_from(temp.path()).await);
    for path in ["../secret.txt", "/etc/passwd", "../../etc/passwd"] {
        let result = tool
            .execute(
                input(json!({ "skill": "pdf-report", "path": path })),
                CancellationToken::new(),
            )
            .await
            .expect("execute");

        assert!(result.is_error, "{path} should have been refused");
        let message = result.output["error"].as_str().expect("error message");
        assert!(
            message.contains("escapes"),
            "{path} produced an unexpected message: {message}"
        );
        assert!(
            !message.contains("classified"),
            "{path} leaked file contents: {message}"
        );
    }
}

#[tokio::test]
async fn descriptors_carry_stable_tool_ids() {
    let temp = TempDir::new().expect("tempdir");
    let catalog = catalog_from(temp.path()).await;

    assert_eq!(
        SkillLoadTool::new(catalog.clone()).descriptor().id.as_str(),
        "skill.load"
    );
    assert_eq!(
        SkillReadTool::new(catalog).descriptor().id.as_str(),
        "skill.read"
    );
}
