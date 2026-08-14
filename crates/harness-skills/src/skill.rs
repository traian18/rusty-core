//! One skill: its metadata, its instructions, and scoped access to the
//! files bundled beside it.

use std::path::{Component, Path, PathBuf};

use crate::error::SkillError;
use crate::frontmatter;

/// The file that makes a directory a skill.
pub const SKILL_FILE: &str = "SKILL.md";

/// Caps on [`Skill::bundled_files`]. A skill directory is authored by hand,
/// so these are generous — they exist to stop a stray `node_modules` or a
/// symlink loop from stalling session start, not to constrain real skills.
const MAX_BUNDLED_FILES: usize = 256;
const MAX_BUNDLED_DEPTH: usize = 4;

/// Where a skill was discovered. Purely informational — it appears in
/// diagnostics and lets a UI show which skills are project-local — but
/// precedence is decided by discovery order, not by this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// `$HOME/.harness/skills`
    User,
    /// `<workspace_root>/.harness/skills`
    Workspace,
    /// A root named explicitly by the embedder or a `--skills-dir` flag.
    Explicit,
}

/// A skill loaded from a `SKILL.md`.
///
/// The split between [`description`](Self::description) and
/// [`instructions`](Self::instructions) is the whole point of the type:
/// descriptions are cheap and always visible in the system prompt, bodies
/// are expensive and only reach the model when it calls `skill.load`.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Lowercase `[a-z0-9-]` identifier, equal to the directory name.
    pub name: String,
    /// One-line summary. The only field that always reaches the prompt.
    pub description: String,
    /// The markdown body after the frontmatter, loaded on demand.
    pub instructions: String,
    /// Optional advisory list of tool ids the skill expects. Not enforced —
    /// tool policy lives in `AgentToolset`, and a skill must not be able to
    /// widen it by declaring a tool the session never granted.
    pub allowed_tools: Vec<String>,
    /// The directory containing `SKILL.md`.
    pub dir: PathBuf,
    pub source: SkillSource,
}

impl Skill {
    /// Loads the skill rooted at `dir`.
    pub async fn load(dir: PathBuf, source: SkillSource) -> Result<Self, SkillError> {
        let path = dir.join(SKILL_FILE);
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|source| SkillError::io(path.clone(), source))?;

        let document = frontmatter::parse(&text)
            .ok_or_else(|| SkillError::MissingFrontmatter { path: path.clone() })?;

        let name = document
            .scalar("name")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SkillError::MissingField {
                path: path.clone(),
                field: "name",
            })?
            .to_string();
        let description = document
            .scalar("description")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SkillError::MissingField {
                path: path.clone(),
                field: "description",
            })?
            .to_string();

        if !is_valid_name(&name) {
            return Err(SkillError::InvalidName { path, name });
        }

        // The directory name is what `skill.load` is called with and what
        // scopes `skill.read`, so a mismatch would make the declared name a
        // lie. Rejecting is better than silently preferring one of them.
        let directory = dir
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if directory != name {
            return Err(SkillError::NameMismatch {
                path,
                declared: name,
                directory,
            });
        }

        Ok(Self {
            name,
            description,
            instructions: document.body.clone(),
            allowed_tools: document.list("allowed-tools"),
            dir,
            source,
        })
    }

    /// Lists files bundled alongside `SKILL.md`, as `/`-separated paths
    /// relative to the skill directory.
    ///
    /// Symlinks are skipped rather than followed: a listing is advisory, and
    /// following links here would hand the model paths that
    /// [`read_bundled`](Self::read_bundled) then refuses, which reads as a
    /// bug from the other side of the tool call.
    pub async fn bundled_files(&self) -> Result<Vec<String>, SkillError> {
        let mut found = Vec::new();
        let mut queue = vec![(self.dir.clone(), 0usize)];

        while let Some((directory, depth)) = queue.pop() {
            let mut entries = tokio::fs::read_dir(&directory)
                .await
                .map_err(|source| SkillError::io(directory.clone(), source))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|source| SkillError::io(directory.clone(), source))?
            {
                if found.len() >= MAX_BUNDLED_FILES {
                    found.sort();
                    return Ok(found);
                }

                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|source| SkillError::io(entry.path(), source))?;
                if file_type.is_symlink() {
                    continue;
                }

                let path = entry.path();
                if file_type.is_dir() {
                    if depth < MAX_BUNDLED_DEPTH {
                        queue.push((path, depth + 1));
                    }
                    continue;
                }

                let Ok(relative) = path.strip_prefix(&self.dir) else {
                    continue;
                };
                let relative = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if relative != SKILL_FILE {
                    found.push(relative);
                }
            }
        }

        found.sort();
        Ok(found)
    }

    /// Reads a file bundled with this skill.
    ///
    /// Scoped to [`dir`](Self::dir). This exists separately from `fs.read`
    /// because skill directories — especially the user-level ones — sit
    /// outside the workspace root, where `FsWorkspace`'s traversal guard
    /// correctly refuses to read.
    pub async fn read_bundled(&self, relative: &str) -> Result<String, SkillError> {
        let path = self.resolve_bundled(relative).await?;
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|source| SkillError::io(path, source))
    }

    /// Resolves `relative` to a real path inside this skill's directory, or
    /// refuses.
    ///
    /// Two checks, both load-bearing:
    ///
    /// 1. Reject absolute paths and any `..` component up front. Cheap, and
    ///    catches the obvious `../../etc/passwd`.
    /// 2. Canonicalize both the skill root and the target, then require the
    ///    target to be prefixed by the root. This is what closes the
    ///    *symlink* escape — a link inside the skill directory pointing
    ///    outside it passes check 1 untouched, because it has no `..` in it.
    async fn resolve_bundled(&self, relative: &str) -> Result<PathBuf, SkillError> {
        let escape = || SkillError::PathEscape {
            skill: self.name.clone(),
            requested: relative.to_string(),
        };

        let requested = Path::new(relative);
        if relative.is_empty() || requested.is_absolute() {
            return Err(escape());
        }
        for component in requested.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(escape())
                }
            }
        }

        let root = tokio::fs::canonicalize(&self.dir)
            .await
            .map_err(|source| SkillError::io(self.dir.clone(), source))?;
        let target = tokio::fs::canonicalize(root.join(requested))
            .await
            .map_err(|_| SkillError::NoSuchFile {
                skill: self.name.clone(),
                requested: relative.to_string(),
            })?;

        if !target.starts_with(&root) {
            return Err(escape());
        }
        Ok(target)
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

#[cfg(test)]
mod tests {
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
}
