//! Wire-serializable skill discovery spec.
//!
//! `harness-skills` owns the real `SkillsConfig` and the discovery that
//! actually walks the filesystem — but that's an I/O-bearing crate, and this
//! one must stay dependency-direction clean (no runtime, no I/O; see
//! `xtask check-deps`), so it can't reuse that type directly. [`SkillsSpec`]
//! is a plain, serializable mirror of the same fields, which is all
//! `RpcRequestBody::CreateSession` needs to carry a skills request over the
//! wire. Whatever host actually creates the session (e.g. `apps/harnessd`'s
//! handler) converts this into a real `harness_skills::SkillsConfig`.
//!
//! Same mirror-don't-reuse discipline as [`McpServerSpec`](crate::mcp::McpServerSpec).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which directories a session should scan for `SKILL.md` files.
///
/// Field-for-field mirror of `harness_skills::SkillsConfig`, except that the
/// workspace root isn't repeated here — `CreateSession` already carries one,
/// and duplicating it would let a client ask for skills from a directory it
/// isn't otherwise allowed to name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSpec {
    /// Scan `$HOME/.harness/skills`. Off by default over the wire: a remote
    /// client asking the daemon to load the *daemon operator's* personal
    /// skills is a decision worth making explicitly, even though the
    /// in-process default in `SkillsConfig` is the more convenient `true`.
    #[serde(default)]
    pub include_user_dir: bool,
    /// Scan `<workspace_root>/.harness/skills`, where `workspace_root` is
    /// the one already given on `CreateSession`.
    #[serde(default = "default_true")]
    pub include_workspace_dir: bool,
    /// Additional roots, scanned last so they win on a name collision.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

impl Default for SkillsSpec {
    fn default() -> Self {
        Self {
            include_user_dir: false,
            include_workspace_dir: true,
            roots: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let spec = SkillsSpec {
            include_user_dir: true,
            include_workspace_dir: false,
            roots: vec![PathBuf::from("/opt/team-skills")],
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let restored: SkillsSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(spec, restored);
    }

    /// An older client that sends `{}` must get the sensible default rather
    /// than a deserialization failure — this is what makes the field
    /// additive on an existing protocol version.
    #[test]
    fn an_empty_object_deserializes_to_the_default() {
        let spec: SkillsSpec = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(spec, SkillsSpec::default());
        assert!(spec.include_workspace_dir);
        assert!(!spec.include_user_dir);
    }
}
