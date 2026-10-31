use std::path::PathBuf;

/// Everything that can go wrong loading or reading a skill.
///
/// Most of these are *per-skill* and non-fatal: [`SkillCatalog::discover`]
/// collects them and keeps going rather than failing session start, so one
/// malformed `SKILL.md` costs its author that skill and nothing else. Only
/// an explicitly configured root that cannot be read is worth surfacing to
/// the caller as a hard error.
///
/// [`SkillCatalog::discover`]: crate::SkillCatalog::discover
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: no `---` frontmatter block")]
    MissingFrontmatter { path: PathBuf },

    #[error("{path}: frontmatter is missing required field `{field}`")]
    MissingField { path: PathBuf, field: &'static str },

    #[error(
        "{path}: frontmatter declares name {declared:?} but the directory is named {directory:?}"
    )]
    NameMismatch {
        path: PathBuf,
        declared: String,
        directory: String,
    },

    #[error(
        "{path}: skill name {name:?} must be lowercase letters, digits, and hyphens (a-z, 0-9, -)"
    )]
    InvalidName { path: PathBuf, name: String },

    #[error("unknown skill {0:?}")]
    UnknownSkill(String),

    #[error("skill {skill:?}: path {requested:?} escapes the skill directory")]
    PathEscape { skill: String, requested: String },

    #[error("skill {skill:?} has no file at {requested:?}")]
    NoSuchFile { skill: String, requested: String },
}

impl SkillError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
