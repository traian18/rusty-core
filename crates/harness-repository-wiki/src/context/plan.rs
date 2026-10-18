//! Chooses which pages a prompt should arrive with: grounded in what the prompt names and what is
//! being worked on, widened by the import graph according to the kind of task, and never guessing.
//! A pure function of the snapshot, the prompt and the focus; no model is called.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use harness_workspace::repository_intelligence::{RepositorySnapshot, RootRelativePath};

use super::{
    centrality::Centrality,
    focus::{FocusSet, TaskKind},
};
use crate::{
    facts::{path_text, Facts},
    model::PageId,
};

/// Most items a plan holds.
pub const MAX_ITEMS: usize = 12;
/// Anchors whose neighbours are followed.
const EXPANDED_ANCHORS: usize = 4;
/// Neighbours taken per anchor and direction.
const NEIGHBOURS: usize = 3;
/// Files matching an identifier before it is too ambiguous to point at any.
const AMBIGUOUS_STEMS: usize = 3;
/// Dirty files treated as anchors; the rest are pointers.
const DIRTY_ANCHORS: usize = 3;

/// How sure the plan is that a page belongs with the prompt.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Tier {
    /// Anchored to something real: the excerpt is injected.
    High,
    /// Related but not anchored: a one-line pointer, so the model decides.
    Medium,
}

/// Why a page was chosen, in priority order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Signal {
    Mentioned,
    NameMatch,
    Active,
    Touched,
    Handoff,
    Dirty,
    Routed,
    Neighbour,
    Surroundings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextItem {
    pub page: PageId,
    pub tier: Tier,
    pub signal: Signal,
    /// Said to the model next to the page.
    pub reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub items: Vec<ContextItem>,
    /// Nothing was anchored, so no excerpts are sent: the onboarding brief, and any ambiguous
    /// matches as pointers.
    pub abstained: bool,
}

/// Which retrieval steps run; all are on by default. The replay turns them off one at a time to see
/// what each one adds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanOptions {
    /// Identifiers that match a file name.
    pub names: bool,
    /// Neighbours and surroundings of the anchors.
    pub expand: bool,
    /// Widening by the kind of task.
    pub route: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            names: true,
            expand: true,
            route: true,
        }
    }
}

pub struct PlanInput<'a> {
    pub message: &'a str,
    pub focus: &'a FocusSet,
    pub task: TaskKind,
}

pub struct ContextPlanner<'a> {
    snapshot: &'a RepositorySnapshot,
    facts: Facts<'a>,
    centrality: Arc<Centrality>,
    by_stem: HashMap<String, Vec<&'a RootRelativePath>>,
    folders: HashSet<String>,
    options: PlanOptions,
}

/// `useViewData`, `use_view_data` and `use-view-data` all become `useviewdata`.
fn stem_key(text: &str) -> String {
    text.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A token that names code (`snake_case`, `camelCase`, `PascalCase`) rather than being a plain word.
fn looks_like_identifier(token: &str) -> bool {
    token.len() >= 5
        && token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        && (token.contains('_')
            || token.chars().skip(1).any(|c| c.is_uppercase())
                && token.chars().any(|c| c.is_lowercase()))
}

fn tokens(message: &str) -> impl Iterator<Item = &str> {
    message
        .split(|c: char| c.is_whitespace() || "\"'`()[]<>,;{}".contains(c))
        .map(|token| token.trim_end_matches(['.', '?', '!', ':']))
        .filter(|token| !token.is_empty())
}

/// `src/a.rs:42:7` and `src/a.rs#L42` name `src/a.rs`.
fn strip_location(token: &str) -> &str {
    let token = token.split('#').next().unwrap_or(token);
    let mut end = token.len();
    for _ in 0..2 {
        match token[..end].rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
                end = head.len()
            }
            _ => break,
        }
    }
    &token[..end]
}

impl<'a> ContextPlanner<'a> {
    pub fn new(snapshot: &'a RepositorySnapshot, centrality: Arc<Centrality>) -> Self {
        let mut by_stem: HashMap<String, Vec<&RootRelativePath>> = HashMap::new();
        let mut folders = HashSet::new();
        for file in &snapshot.imports.files {
            let path = file.path.as_path();
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !matches!(stem, "mod" | "index" | "lib" | "main" | "__init__") {
                    by_stem.entry(stem_key(stem)).or_default().push(&file.path);
                }
            }
            for ancestor in path.ancestors().skip(1) {
                if !ancestor.as_os_str().is_empty() {
                    folders.insert(ancestor.display().to_string());
                }
            }
        }
        Self {
            snapshot,
            facts: Facts::new(snapshot),
            centrality,
            by_stem,
            folders,
            options: PlanOptions::default(),
        }
    }

    pub fn with_options(mut self, options: PlanOptions) -> Self {
        self.options = options;
        self
    }

    fn file(&self, path: &str) -> Option<RootRelativePath> {
        let path = RootRelativePath::new(path.trim_start_matches("./")).ok()?;
        self.facts.file(&path).map(|f| f.path.clone())
    }

    pub fn plan(&self, input: &PlanInput<'_>) -> Plan {
        let mut chosen: Vec<ContextItem> = Vec::new();
        let mut anchors: Vec<RootRelativePath> = Vec::new();
        let add = |chosen: &mut Vec<ContextItem>, item: ContextItem| match chosen
            .iter_mut()
            .find(|c| c.page == item.page)
        {
            Some(existing) if (item.tier, item.signal) < (existing.tier, existing.signal) => {
                *existing = item
            }
            Some(_) => {}
            None => chosen.push(item),
        };

        // 1. Anchors named in the message: exact paths and folders.
        for token in tokens(input.message) {
            let token = strip_location(token);
            if let Some(file) = self.file(token) {
                if !anchors.contains(&file) {
                    anchors.push(file.clone());
                }
                add(
                    &mut chosen,
                    ContextItem {
                        page: PageId::file(&path_text(&file)),
                        tier: Tier::High,
                        signal: Signal::Mentioned,
                        reason: format!("you mentioned `{}`", path_text(&file)),
                    },
                );
            } else if self.folders.contains(token.trim_end_matches('/')) {
                let folder = token.trim_end_matches('/');
                add(
                    &mut chosen,
                    ContextItem {
                        page: PageId::folder(&format!("dir:{folder}")),
                        tier: Tier::High,
                        signal: Signal::Mentioned,
                        reason: format!("you mentioned the folder `{folder}`"),
                    },
                );
            }
        }
        // 2. Identifiers that match a file name.
        for token in
            tokens(input.message).filter(|t| self.options.names && looks_like_identifier(t))
        {
            let Some(matches) = self.by_stem.get(&stem_key(token)) else {
                continue;
            };
            if matches.len() > AMBIGUOUS_STEMS {
                continue;
            }
            let tier = if matches.len() == 1 {
                Tier::High
            } else {
                Tier::Medium
            };
            for file in matches {
                if tier == Tier::High && !anchors.contains(file) {
                    anchors.push((*file).clone());
                }
                add(
                    &mut chosen,
                    ContextItem {
                        page: PageId::file(&path_text(file)),
                        tier,
                        signal: Signal::NameMatch,
                        reason: format!("`{token}` matches the file name"),
                    },
                );
            }
        }
        // 3. What is being worked on.
        let focus = input.focus;
        let focus_files = |names: &[String]| {
            names
                .iter()
                .filter_map(|n| self.file(n))
                .collect::<Vec<_>>()
        };
        let mut focused: Vec<(RootRelativePath, Signal, &str, Tier)> = Vec::new();
        if let Some(active) = focus.active_file.as_deref().and_then(|n| self.file(n)) {
            focused.push((
                active,
                Signal::Active,
                "it is open in the editor",
                Tier::High,
            ));
        }
        focused.extend(focus_files(&focus.touched).into_iter().map(|f| {
            (
                f,
                Signal::Touched,
                "it was read or edited in this chat",
                Tier::High,
            )
        }));
        focused.extend(focus_files(&focus.handoff).into_iter().map(|f| {
            (
                f,
                Signal::Handoff,
                "an earlier step of this workflow handled it",
                Tier::High,
            )
        }));
        focused.extend(
            focus_files(&focus.dirty)
                .into_iter()
                .enumerate()
                .map(|(i, f)| {
                    (
                        f,
                        Signal::Dirty,
                        "it has uncommitted changes",
                        if i < DIRTY_ANCHORS {
                            Tier::High
                        } else {
                            Tier::Medium
                        },
                    )
                }),
        );
        for (file, signal, why, tier) in focused {
            if tier == Tier::High && !anchors.contains(&file) {
                anchors.push(file.clone());
            }
            add(
                &mut chosen,
                ContextItem {
                    page: PageId::file(&path_text(&file)),
                    tier,
                    signal,
                    reason: why.to_owned(),
                },
            );
        }

        // Nothing anchored: no excerpts. Ambiguous matches may still be offered as pointers.
        if anchors.is_empty() {
            chosen.retain(|item| item.tier == Tier::Medium);
            chosen.sort_by(|a, b| a.signal.cmp(&b.signal).then(a.page.cmp(&b.page)));
            chosen.truncate(MAX_ITEMS);
            return Plan {
                items: chosen,
                abstained: true,
            };
        }

        // 4. Widen along the import graph, by task.
        for anchor in anchors.iter().take(if self.options.expand {
            EXPANDED_ANCHORS
        } else {
            0
        }) {
            let (importers, imported) = (self.importers(anchor), self.imported(anchor));
            let name = path_text(anchor);
            let (promote_importers, promote_imports) = match if self.options.route {
                input.task
            } else {
                TaskKind::General
            } {
                TaskKind::Refactor | TaskKind::Review => (true, false),
                TaskKind::Debug => (false, true),
                _ => (false, false),
            };
            for file in importers.into_iter().take(NEIGHBOURS) {
                let (tier, signal) = if promote_importers {
                    (Tier::High, Signal::Routed)
                } else {
                    (Tier::Medium, Signal::Neighbour)
                };
                let reason = if promote_importers {
                    format!("it imports `{name}`, so a change there may break it")
                } else {
                    format!("it imports `{name}`")
                };
                add(
                    &mut chosen,
                    ContextItem {
                        page: PageId::file(&path_text(&file)),
                        tier,
                        signal,
                        reason,
                    },
                );
            }
            for file in imported.into_iter().take(NEIGHBOURS) {
                let (tier, signal) = if promote_imports {
                    (Tier::High, Signal::Routed)
                } else {
                    (Tier::Medium, Signal::Neighbour)
                };
                let reason = if promote_imports {
                    format!("`{name}` calls into it")
                } else {
                    format!("`{name}` imports it")
                };
                add(
                    &mut chosen,
                    ContextItem {
                        page: PageId::file(&path_text(&file)),
                        tier,
                        signal,
                        reason,
                    },
                );
            }
        }
        // 5. The folder around the first anchor.
        if let Some(parent) = anchors
            .first()
            .and_then(|a| a.as_path().parent())
            .filter(|p| self.options.expand && !p.as_os_str().is_empty())
        {
            let (tier, why) = if matches!(input.task, TaskKind::Plan | TaskKind::Document) {
                (Tier::High, "the folder of the file you are working on")
            } else {
                (Tier::Medium, "the folder of the file you are working on")
            };
            add(
                &mut chosen,
                ContextItem {
                    page: PageId::folder(&format!("dir:{}", parent.display())),
                    tier,
                    signal: Signal::Surroundings,
                    reason: why.to_owned(),
                },
            );
        }

        chosen.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then(a.signal.cmp(&b.signal))
                .then_with(|| self.rank(&b.page).total_cmp(&self.rank(&a.page)))
                .then(a.page.cmp(&b.page))
        });
        chosen.truncate(MAX_ITEMS);
        Plan {
            items: chosen,
            abstained: false,
        }
    }

    /// Centrality of a file page; other levels rank neutral.
    fn rank(&self, page: &PageId) -> f64 {
        page.0
            .strip_prefix("file:")
            .and_then(|p| RootRelativePath::new(p).ok())
            .map_or(0.0, |p| self.centrality.score(&p))
    }

    fn importers(&self, file: &RootRelativePath) -> Vec<RootRelativePath> {
        let mut files: Vec<RootRelativePath> = self
            .facts
            .dependent_files(file)
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        files.sort_by(|a, b| {
            self.centrality
                .score(b)
                .total_cmp(&self.centrality.score(a))
                .then(a.cmp(b))
        });
        files
    }

    fn imported(&self, file: &RootRelativePath) -> Vec<RootRelativePath> {
        let mut files: Vec<RootRelativePath> = self
            .facts
            .imports_of(file)
            .iter()
            .filter_map(|e| e.resolution.target_file().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        files.sort_by(|a, b| {
            self.centrality
                .score(b)
                .total_cmp(&self.centrality.score(a))
                .then(a.cmp(b))
        });
        files
    }

    pub fn snapshot(&self) -> &RepositorySnapshot {
        self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_matched_without_regard_to_case_or_separators() {
        assert_eq!(stem_key("useViewData"), stem_key("use_view_data"));
        assert_eq!(stem_key("use-view-data"), "useviewdata");
        assert!(looks_like_identifier("useViewData"));
        assert!(looks_like_identifier("view_data"));
        assert!(
            !looks_like_identifier("service"),
            "a plain word is not an identifier"
        );
        assert!(!looks_like_identifier("Fix"));
    }

    #[test]
    fn a_location_suffix_is_not_part_of_the_path() {
        assert_eq!(strip_location("src/a.rs:42:7"), "src/a.rs");
        assert_eq!(strip_location("src/a.rs#L12"), "src/a.rs");
        assert_eq!(strip_location("src/a.rs"), "src/a.rs");
        assert_eq!(strip_location("c:"), "c:");
    }
}
