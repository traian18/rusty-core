use super::*;

use tempfile::TempDir;

/// Writes a skill directory and returns its path.
async fn write_skill(root: &Path, name: &str, contents: &str) -> PathBuf {
    let dir = root.join(name);
    tokio::fs::create_dir_all(&dir).await.expect("create dir");
    tokio::fs::write(dir.join(SKILL_FILE), contents)
        .await
        .expect("write SKILL.md");
    dir
}

fn valid_skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Does a thing.\n---\nStep one.\n")
}

#[tokio::test]
async fn loads_metadata_and_body() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "pdf-report", &valid_skill("pdf-report")).await;

    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("skill should load");
    assert_eq!(skill.name, "pdf-report");
    assert_eq!(skill.description, "Does a thing.");
    assert_eq!(skill.instructions, "Step one.\n");
}

#[tokio::test]
async fn rejects_missing_required_fields() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "broken", "---\nname: broken\n---\nBody.\n").await;

    let error = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect_err("missing description must fail");
    assert!(matches!(
        error,
        SkillError::MissingField {
            field: "description",
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_frontmatterless_files() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "plain", "# Just markdown\n").await;

    let error = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect_err("missing frontmatter must fail");
    assert!(matches!(error, SkillError::MissingFrontmatter { .. }));
}

#[tokio::test]
async fn rejects_a_name_that_disagrees_with_the_directory() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "on-disk", &valid_skill("in-frontmatter")).await;

    let error = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect_err("name mismatch must fail");
    assert!(matches!(error, SkillError::NameMismatch { .. }));
}

#[tokio::test]
async fn rejects_names_outside_the_allowed_alphabet() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "Bad Name", &valid_skill("Bad Name")).await;

    let error = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect_err("invalid name must fail");
    assert!(matches!(error, SkillError::InvalidName { .. }));
}

#[tokio::test]
async fn lists_bundled_files_excluding_the_manifest() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "bundle", &valid_skill("bundle")).await;
    tokio::fs::write(dir.join("template.tex"), "x")
        .await
        .expect("write");
    tokio::fs::create_dir_all(dir.join("assets"))
        .await
        .expect("mkdir");
    tokio::fs::write(dir.join("assets").join("logo.svg"), "x")
        .await
        .expect("write");

    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("load");
    let files = skill.bundled_files().await.expect("list");
    assert_eq!(
        files,
        vec!["assets/logo.svg".to_string(), "template.tex".to_string()]
    );
}

#[tokio::test]
async fn reads_a_bundled_file() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "bundle", &valid_skill("bundle")).await;
    tokio::fs::write(dir.join("template.tex"), "\\documentclass{article}")
        .await
        .expect("write");

    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("load");
    let content = skill.read_bundled("template.tex").await.expect("read");
    assert_eq!(content, "\\documentclass{article}");
}

#[tokio::test]
async fn rejects_parent_traversal_and_absolute_paths() {
    let temp = TempDir::new().expect("tempdir");
    tokio::fs::write(temp.path().join("secret.txt"), "classified")
        .await
        .expect("write");
    let dir = write_skill(temp.path(), "bundle", &valid_skill("bundle")).await;
    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("load");

    for attempt in ["../secret.txt", "../../etc/passwd", "/etc/passwd", ""] {
        let error = skill
            .read_bundled(attempt)
            .await
            .expect_err("traversal must be refused");
        assert!(
            matches!(error, SkillError::PathEscape { .. }),
            "{attempt:?} produced {error:?}"
        );
    }
}

/// The case a `..`-component check alone would miss: a symlink *inside*
/// the skill directory whose target is outside it. Only canonicalizing
/// catches this.
#[cfg(unix)]
#[tokio::test]
async fn rejects_a_symlink_escaping_the_skill_directory() {
    let temp = TempDir::new().expect("tempdir");
    let secret = temp.path().join("secret.txt");
    tokio::fs::write(&secret, "classified")
        .await
        .expect("write");
    let dir = write_skill(temp.path(), "bundle", &valid_skill("bundle")).await;
    std::os::unix::fs::symlink(&secret, dir.join("escape.txt")).expect("symlink");

    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("load");
    let error = skill
        .read_bundled("escape.txt")
        .await
        .expect_err("symlink escape must be refused");
    assert!(matches!(error, SkillError::PathEscape { .. }), "{error:?}");
}

#[tokio::test]
async fn reports_a_missing_bundled_file_distinctly_from_an_escape() {
    let temp = TempDir::new().expect("tempdir");
    let dir = write_skill(temp.path(), "bundle", &valid_skill("bundle")).await;
    let skill = Skill::load(dir, SkillSource::Workspace)
        .await
        .expect("load");

    let error = skill
        .read_bundled("absent.txt")
        .await
        .expect_err("missing file must fail");
    assert!(matches!(error, SkillError::NoSuchFile { .. }), "{error:?}");
}
