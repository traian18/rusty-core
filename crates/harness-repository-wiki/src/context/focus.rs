//! What the person or run is working on, supplied by the host. Core knows nothing about editors,
//! chats or workflows: it only reads this set.
use std::sync::Arc;

/// Files the host says are in play, as workspace-relative paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FocusSet {
    /// The file open in the editor.
    pub active_file: Option<String>,
    /// Files read or modified earlier in this chat.
    pub touched: Vec<String>,
    /// Files Git reports as changed in the working tree.
    pub dirty: Vec<String>,
    /// Files earlier steps of a workflow read or changed.
    pub handoff: Vec<String>,
}

impl FocusSet {
    pub fn is_empty(&self) -> bool {
        self.active_file.is_none()
            && self.touched.is_empty()
            && self.dirty.is_empty()
            && self.handoff.is_empty()
    }
}

/// Supplies the focus for the next prompt. Implemented by the host.
pub trait FocusSource: Send + Sync {
    fn focus(&self) -> FocusSet;
}

/// A fixed focus: what a host knew when it built the session.
pub struct StaticFocus(pub FocusSet);

impl FocusSource for StaticFocus {
    fn focus(&self) -> FocusSet {
        self.0.clone()
    }
}

impl FocusSource for Arc<dyn FocusSource> {
    fn focus(&self) -> FocusSet {
        self.as_ref().focus()
    }
}

/// The kind of work a prompt belongs to, taken from the running profile or workflow step. It decides
/// how far from the anchors the context reaches, never which model runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TaskKind {
    #[default]
    General,
    /// Changing structure: what depends on the files matters most.
    Refactor,
    /// Finding a fault: what the files call into matters most.
    Debug,
    /// Adding behaviour: similar existing code and the surroundings.
    Feature,
    /// Judging a change: what the changed files reach.
    Review,
    /// Understanding or designing: folders and overviews.
    Plan,
    Document,
}

impl TaskKind {
    /// The task for a profile id such as `refactor` or `rusty-ide.builtin.debug`.
    pub fn from_profile(id: &str) -> Self {
        let name = id.rsplit('.').next().unwrap_or(id);
        let name = name.split('@').next().unwrap_or(name);
        match name {
            "refactor" | "optimize" => Self::Refactor,
            "debug" => Self::Debug,
            "build" | "implement" => Self::Feature,
            "review" | "verify" | "security" => Self::Review,
            "plan" | "architect" | "analyze" | "research" => Self::Plan,
            "document" => Self::Document,
            _ => Self::General,
        }
    }
}
