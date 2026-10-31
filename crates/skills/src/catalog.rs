//! Discovery: turning a set of directories on disk into a [`SkillCatalog`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::error::SkillError;
use crate::skill::{Skill, SkillSource, SKILL_FILE};

/// The per-project state directory. Matches where sessions are already
/// persisted (`apps/harnessd`'s `--sessions-dir` default and
/// `AppHarness::new`), so a project has one harness directory rather than
/// two competing conventions.
pub const HARNESS_DIR: &str = ".harness";
/// The subdirectory of [`HARNESS_DIR`] scanned for skills.
pub const SKILLS_DIR: &str = "skills";

/// Which directories to scan for skills.
///
/// Defaults to "user skills, plus this workspace's" — set
/// [`workspace_root`](Self::workspace_root) to enable the second.
#[derive(Debug, Clone)]
pub struct SkillsConfig {
    /// Scans `<workspace_root>/.harness/skills` when set.
    pub workspace_root: Option<PathBuf>,
    /// Scans `$HOME/.harness/skills`.
    pub include_user_dir: bool,
    /// Additional roots, scanned last so they win on a name collision.
    pub extra_roots: Vec<PathBuf>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            workspace_root: None,
            include_user_dir: true,
            extra_roots: Vec::new(),
        }
    }
}

impl SkillsConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    pub fn include_user_dir(mut self, include: bool) -> Self {
        self.include_user_dir = include;
        self
    }

    /// Add an explicit root (e.g. one `--skills-dir` flag).
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.extra_roots.push(root.into());
        self
    }

    /// The roots to scan, in increasing precedence order.
    ///
    /// User skills are the broadest default, a project's own skills should
    /// be able to override them, and an explicitly named root is the most
    /// deliberate thing the caller can say — so it wins.
    fn roots(&self) -> Vec<(PathBuf, SkillSource)> {
        let mut roots = Vec::new();
        if self.include_user_dir {
            if let Some(home) = home_dir() {
                roots.push((home.join(HARNESS_DIR).join(SKILLS_DIR), SkillSource::User));
            }
        }
        if let Some(workspace_root) = &self.workspace_root {
            roots.push((
                workspace_root.join(HARNESS_DIR).join(SKILLS_DIR),
                SkillSource::Workspace,
            ));
        }
        for root in &self.extra_roots {
            roots.push((root.clone(), SkillSource::Explicit));
        }
        roots
    }
}

/// Every skill available to a session, keyed by name.
///
/// `BTreeMap` rather than `HashMap` so [`catalog_prompt`](Self::catalog_prompt)
/// is byte-stable across runs — it lands in the system prompt, where a
/// reordering would defeat prompt caching for no reason.
#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    skills: BTreeMap<String, Skill>,
}

impl SkillCatalog {
    /// Scans every configured root.
    ///
    /// Returns the catalog **and** the problems found, rather than failing
    /// on the first one: a single malformed `SKILL.md` should cost its
    /// author that one skill, not prevent the session from starting. Callers
    /// are expected to log the errors. A root that does not exist is not an
    /// error — `$HOME/.harness/skills` is absent on most machines.
    pub async fn discover(config: &SkillsConfig) -> (Self, Vec<SkillError>) {
        let mut skills = BTreeMap::new();
        let mut errors = Vec::new();

        for (root, source) in config.roots() {
            scan(&root, source, &mut skills, &mut errors).await;
        }

        (Self { skills }, errors)
    }

    /// Build a catalog directly from loaded skills. Useful for embedders
    /// that source skills from somewhere other than the filesystem.
    pub fn from_skills(skills: impl IntoIterator<Item = Skill>) -> Self {
        Self {
            skills: skills
                .into_iter()
                .map(|skill| (skill.name.clone(), skill))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// The catalog as it appears in the system prompt: one line per skill,
    /// name and description only.
    ///
    /// This is the progressive-disclosure boundary. Instruction bodies are
    /// deliberately absent — they reach the model only when it calls
    /// `skill.load`, which is what keeps a directory of thirty skills from
    /// costing thirty skills' worth of tokens on every request.
    ///
    /// Returns an empty string when there are no skills, so a session
    /// without any pays nothing at all.
    pub fn catalog_prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut prompt = String::from(
            "# Available skills\n\n\
             Each entry below is a set of instructions for one kind of task. When a \
             request matches an entry's description, call the `skill.load` tool with \
             that skill's name to read its full instructions before doing the work. \
             Use `skill.read` to read any files the skill bundles.\n\n",
        );
        for skill in self.skills.values() {
            prompt.push_str("- `");
            prompt.push_str(&skill.name);
            prompt.push_str("`: ");
            prompt.push_str(skill.description.trim());
            prompt.push('\n');
        }
        prompt
    }
}

async fn scan(
    root: &Path,
    source: SkillSource,
    skills: &mut BTreeMap<String, Skill>,
    errors: &mut Vec<SkillError>,
) {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        // A root that simply isn't there is the common case, not a problem.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            debug!(root = %root.display(), "skills: no such root, skipping");
            return;
        }
        Err(error) => {
            errors.push(SkillError::io(root, error));
            return;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                errors.push(SkillError::io(root, error));
                return;
            }
        };

        let path = entry.path();
        if !tokio::fs::metadata(&path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        // A directory without a SKILL.md isn't a broken skill, it's not a
        // skill — say nothing about it.
        if tokio::fs::metadata(path.join(SKILL_FILE)).await.is_err() {
            continue;
        }

        match Skill::load(path, source).await {
            Ok(skill) => {
                skills.insert(skill.name.clone(), skill);
            }
            Err(error) => errors.push(error),
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    async fn write_skill(root: &Path, name: &str, description: &str) {
        let dir = root.join(name);
        tokio::fs::create_dir_all(&dir).await.expect("create dir");
        tokio::fs::write(
            dir.join(SKILL_FILE),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody for {name}.\n"),
        )
        .await
        .expect("write SKILL.md");
    }

    fn config_with_roots(roots: Vec<PathBuf>) -> SkillsConfig {
        SkillsConfig {
            workspace_root: None,
            include_user_dir: false,
            extra_roots: roots,
        }
    }

    #[tokio::test]
    async fn discovers_skills_from_an_explicit_root() {
        let temp = TempDir::new().expect("tempdir");
        write_skill(temp.path(), "alpha", "First skill.").await;
        write_skill(temp.path(), "beta", "Second skill.").await;

        let (catalog, errors) =
            SkillCatalog::discover(&config_with_roots(vec![temp.path().to_path_buf()])).await;
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(catalog.len(), 2);
        assert_eq!(
            catalog.get("alpha").map(|skill| skill.description.as_str()),
            Some("First skill.")
        );
    }

    #[tokio::test]
    async fn a_later_root_overrides_an_earlier_one() {
        let first = TempDir::new().expect("tempdir");
        let second = TempDir::new().expect("tempdir");
        write_skill(first.path(), "shared", "From the first root.").await;
        write_skill(second.path(), "shared", "From the second root.").await;

        let (catalog, errors) = SkillCatalog::discover(&config_with_roots(vec![
            first.path().to_path_buf(),
            second.path().to_path_buf(),
        ]))
        .await;
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            catalog
                .get("shared")
                .map(|skill| skill.description.as_str()),
            Some("From the second root.")
        );
    }

    #[tokio::test]
    async fn a_malformed_skill_is_reported_but_its_siblings_still_load() {
        let temp = TempDir::new().expect("tempdir");
        write_skill(temp.path(), "good", "Fine.").await;
        let broken = temp.path().join("broken");
        tokio::fs::create_dir_all(&broken).await.expect("mkdir");
        tokio::fs::write(broken.join(SKILL_FILE), "no frontmatter here\n")
            .await
            .expect("write");

        let (catalog, errors) =
            SkillCatalog::discover(&config_with_roots(vec![temp.path().to_path_buf()])).await;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(matches!(errors[0], SkillError::MissingFrontmatter { .. }));
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("good").is_some());
    }

    #[tokio::test]
    async fn a_missing_root_is_silent() {
        let temp = TempDir::new().expect("tempdir");
        let absent = temp.path().join("does-not-exist");

        let (catalog, errors) = SkillCatalog::discover(&config_with_roots(vec![absent])).await;
        assert!(errors.is_empty(), "{errors:?}");
        assert!(catalog.is_empty());
    }

    #[tokio::test]
    async fn directories_without_a_manifest_are_ignored() {
        let temp = TempDir::new().expect("tempdir");
        tokio::fs::create_dir_all(temp.path().join("not-a-skill"))
            .await
            .expect("mkdir");
        write_skill(temp.path(), "real", "Yes.").await;

        let (catalog, errors) =
            SkillCatalog::discover(&config_with_roots(vec![temp.path().to_path_buf()])).await;
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(catalog.len(), 1);
    }

    #[tokio::test]
    async fn the_catalog_prompt_carries_descriptions_but_never_bodies() {
        let temp = TempDir::new().expect("tempdir");
        write_skill(temp.path(), "alpha", "First skill.").await;

        let (catalog, _) =
            SkillCatalog::discover(&config_with_roots(vec![temp.path().to_path_buf()])).await;
        let prompt = catalog.catalog_prompt();
        assert!(prompt.contains("`alpha`"), "{prompt}");
        assert!(prompt.contains("First skill."), "{prompt}");
        assert!(
            !prompt.contains("Body for alpha."),
            "instruction bodies must not reach the system prompt: {prompt}"
        );
    }

    #[test]
    fn an_empty_catalog_produces_an_empty_prompt() {
        assert!(SkillCatalog::default().catalog_prompt().is_empty());
    }

    #[test]
    fn explicit_roots_take_precedence_over_the_workspace_root() {
        let config = SkillsConfig::new()
            .include_user_dir(false)
            .workspace_root("/project")
            .root("/explicit");
        let roots = config.roots();
        assert_eq!(
            roots
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("/project").join(HARNESS_DIR).join(SKILLS_DIR),
                PathBuf::from("/explicit"),
            ]
        );
    }
}
