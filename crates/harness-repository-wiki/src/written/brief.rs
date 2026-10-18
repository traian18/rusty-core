//! What a writer is given for one page: the page's shape, facts as plain text, and real code. The
//! brief is built from the index and the files alone, so the same repository gives the same brief.
use std::{collections::HashMap, sync::Arc};

use harness_workspace::{
    dependency_groups::{Group, GroupKind},
    folder_tree,
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};
use sha2::{Digest, Sha256};

use super::{
    diagram::{dependency_diagram, limit_nodes, Diagram, DiagramNode},
    evidence::Evidence,
    freshness::freshness,
    glossary,
    ground::{ground, Known},
    model::{Author, Freshness, InputFile, Source, SourceRef, WrittenKind, WrittenPage},
    outline,
    pages::{self, Body, MAX_COMPONENTS},
};
use crate::{
    error::WikiError,
    facts::path_text,
    model::{Citation, PageId, PageLevel},
    render::PageRenderer,
};

pub const OVERVIEW: &str = "written:overview";
pub const GETTING_STARTED: &str = "written:getting_started";
pub const KEY_CONCEPTS: &str = "written:key_concepts";
pub const HOW_IT_WORKS: &str = "written:how_it_works";
pub const ARCHITECTURE: &str = "written:architecture";
pub const ACTIVE_LANDSCAPE: &str = "written:active_landscape";
pub const GLOSSARY: &str = "written:glossary";

/// The pages that start a reader off, in reading order.
const START_PAGES: [(&str, WrittenKind, &str); 7] = [
    (OVERVIEW, WrittenKind::Overview, "Overview"),
    (
        GETTING_STARTED,
        WrittenKind::GettingStarted,
        "Getting started",
    ),
    (KEY_CONCEPTS, WrittenKind::KeyConcepts, "Key concepts"),
    (HOW_IT_WORKS, WrittenKind::HowItWorks, "How it works"),
    (ARCHITECTURE, WrittenKind::Architecture, "Architecture"),
    (
        ACTIVE_LANDSCAPE,
        WrittenKind::ActiveLandscape,
        "Active landscape",
    ),
    (GLOSSARY, WrittenKind::Glossary, "Glossary"),
];
/// More demoted references than this, or fewer words, and the page is marked for review.
const REVIEW_DEMOTED: usize = 6;
const REVIEW_WORDS: usize = 150;
/// Part of every facts digest: bumping it marks every page written under older outlines as stale,
/// so the next "Write documentation" rewrites them.
const OUTLINE_VERSION: &str = "outlines-v2";

/// How a page reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Style {
    /// Terse and complete.
    Reference,
    /// Explains why things are the way they are.
    #[default]
    Comprehensive,
    /// Ordered by what a newcomer needs first.
    Tutorial,
}

impl Style {
    pub fn parse(name: &str) -> Self {
        match name {
            "reference" => Self::Reference,
            "tutorial" => Self::Tutorial,
            _ => Self::Comprehensive,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Comprehensive => "comprehensive",
            Self::Tutorial => "tutorial",
        }
    }
}

/// What the caller knows that the index does not.
#[derive(Clone, Debug, Default)]
pub struct BriefContext {
    pub repository_name: String,
    pub style: Style,
    /// Earlier pages of this repository by id, for staying consistent (not copied).
    pub written: HashMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub id: String,
    pub kind: WrittenKind,
    pub title: String,
    pub files: usize,
}

#[derive(Debug)]
pub struct Brief {
    pub id: String,
    pub kind: WrittenKind,
    pub title: String,
    pub system: String,
    pub prompt: String,
    pub diagrams: Vec<Diagram>,
    pub inputs: Vec<InputFile>,
    pub facts_digest: String,
    pub input_chars: usize,
    /// A page built without a model (the glossary): save this text as written.
    pub prewritten: Option<String>,
    /// Every repository path, for grounding what the writer names.
    pub known_paths: Vec<String>,
}

pub struct BriefBuilder {
    pages: PageRenderer,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn start_page(id: &str) -> Option<(WrittenKind, &'static str)> {
    START_PAGES
        .iter()
        .find(|(page, ..)| *page == id)
        .map(|(_, kind, title)| (*kind, *title))
}

impl BriefBuilder {
    pub fn new(pages: PageRenderer) -> Self {
        Self { pages }
    }

    pub(super) fn pages(&self) -> &PageRenderer {
        &self.pages
    }

    fn snapshot(&self) -> Result<Arc<RepositorySnapshot>, WikiError> {
        self.pages.current_snapshot()
    }

    /// The top-level components: the declared packages, or the top-level folders. A repository that
    /// is one package is described by what is inside it.
    pub(super) fn components(snapshot: &RepositorySnapshot) -> Vec<Group> {
        let (_, mut top) = folder_tree::top_level(snapshot);
        if top.len() == 1 {
            let only = top.remove(0);
            let mut inside: Vec<Group> = folder_tree::list(&only)
                .folders
                .into_iter()
                .map(|f| f.group)
                .collect();
            inside.insert(0, only);
            top = inside;
        }
        top.sort_by(|a, b| b.members.len().cmp(&a.members.len()).then(a.id.cmp(&b.id)));
        top
    }

    pub(super) fn page_id(group: &Group) -> PageId {
        match group.kind {
            GroupKind::DeclaredPackage | GroupKind::SourceRoot => {
                PageId::new(PageLevel::Package, &group.id.0)
            }
            _ => PageId::folder(&group.id.0),
        }
    }

    fn module_group(snapshot: &RepositorySnapshot, id: &str) -> Result<Group, WikiError> {
        let page_id = PageId(id.to_owned());
        let (level, target) = page_id.parse()?;
        if !matches!(level, PageLevel::Package | PageLevel::Folder) {
            return Err(WikiError::InvalidId(format!("{id}: only modules (folders and packages) and the documentation pages can be written")));
        }
        folder_tree::group_of(snapshot, target).ok_or_else(|| WikiError::InvalidId(id.to_owned()))
    }

    fn glossary_terms(&self) -> Vec<glossary::Term> {
        let mut evidence = Evidence::new(self.pages.graph().as_ref());
        pages::glossary_terms(&mut evidence)
    }

    /// What can be written now: the pages that start a reader off (the active landscape only with
    /// Git history, the glossary only when the repository defines enough terms), then the
    /// top-level modules.
    pub fn targets(&self) -> Result<Vec<Target>, WikiError> {
        let snapshot = self.snapshot()?;
        let files = snapshot.imports.files.len();
        let history = self
            .pages
            .git_snapshot()
            .is_some_and(|g| !g.index.signals.is_empty());
        let glossary = self.glossary_terms().len() >= glossary::MIN_TERMS;
        let mut out: Vec<Target> = START_PAGES
            .iter()
            .filter(|(id, ..)| {
                (*id != ACTIVE_LANDSCAPE || history) && (*id != GLOSSARY || glossary)
            })
            .map(|(id, kind, title)| Target {
                id: (*id).to_owned(),
                kind: *kind,
                title: (*title).to_owned(),
                files,
            })
            .collect();
        for group in Self::components(&snapshot).into_iter().take(MAX_COMPONENTS) {
            out.push(Target {
                id: Self::page_id(&group).0,
                kind: WrittenKind::Module,
                title: group.label.clone(),
                files: group.members.len(),
            });
        }
        Ok(out)
    }

    /// A digest of the facts a page is written from: equal digests mean nothing it rests on changed.
    /// Changes to the files it read are caught separately, by their content hashes.
    pub fn facts_digest(&self, id: &str) -> Result<String, WikiError> {
        let mut hasher = Sha256::new();
        hasher.update(OUTLINE_VERSION.as_bytes());
        let snapshot = self.snapshot()?;
        match start_page(id) {
            Some((kind, _)) => {
                hasher.update(
                    self.pages
                        .page(&PageId::repository(), None)?
                        .digest
                        .as_bytes(),
                );
                if matches!(kind, WrittenKind::Overview | WrittenKind::Architecture) {
                    for group in Self::components(&snapshot).into_iter().take(MAX_COMPONENTS) {
                        hasher.update(
                            self.pages
                                .page(&Self::page_id(&group), None)?
                                .digest
                                .as_bytes(),
                        );
                    }
                }
                if kind == WrittenKind::ActiveLandscape {
                    hasher.update(
                        self.pages
                            .git_snapshot()
                            .map(|g| g.id.0.clone())
                            .unwrap_or_default()
                            .as_bytes(),
                    );
                }
            }
            None => hasher.update(
                self.pages
                    .page(&PageId(id.to_owned()), None)?
                    .digest
                    .as_bytes(),
            ),
        }
        Ok(hex(&hasher.finalize()))
    }

    pub fn brief(&self, id: &str, context: &BriefContext) -> Result<Brief, WikiError> {
        let snapshot = self.snapshot()?;
        let mut evidence = Evidence::new(self.pages.graph().as_ref());
        let (kind, title, body): (WrittenKind, String, Body) = match start_page(id) {
            Some((kind, title)) => {
                let body = match kind {
                    WrittenKind::Overview => {
                        pages::overview(self, &snapshot, context, &mut evidence)?
                    }
                    WrittenKind::GettingStarted => {
                        pages::getting_started(&snapshot, &mut evidence)?
                    }
                    WrittenKind::KeyConcepts => {
                        pages::key_concepts(&snapshot, context, &mut evidence)?
                    }
                    WrittenKind::HowItWorks => {
                        pages::how_it_works(&snapshot, context, &mut evidence)?
                    }
                    WrittenKind::Architecture => {
                        pages::architecture(self, &snapshot, context, &mut evidence)?
                    }
                    WrittenKind::ActiveLandscape => {
                        pages::active_landscape(self, &snapshot, &mut evidence)?
                    }
                    _ => pages::glossary(context, &mut evidence)?,
                };
                (kind, title.to_owned(), body)
            }
            None => {
                let group = Self::module_group(&snapshot, id)?;
                let body = pages::module(self, &snapshot, context, &group, &mut evidence)?;
                (WrittenKind::Module, group.label.clone(), body)
            }
        };
        let diagrams = if body.diagram {
            self.diagrams(id)?
        } else {
            Vec::new()
        };
        let keys: Vec<String> = diagrams.iter().map(|d| d.key.clone()).collect();
        let system = outline::system(outline::style_guidance(context.style.name()), &keys);
        let subject = if kind == WrittenKind::Module {
            title.clone()
        } else {
            context.repository_name.clone()
        };
        let prompt = format!(
            "{}\n\n# Facts\n{}\n\n# Code excerpts\n{}",
            outline::instructions(kind, &subject, !keys.is_empty()),
            body.facts,
            evidence.render()
        );
        let mut known_paths: Vec<String> = snapshot
            .imports
            .files
            .iter()
            .map(|f| path_text(&f.path))
            .collect();
        known_paths.extend(evidence.inputs().into_iter().map(|i| i.file));
        known_paths.sort();
        known_paths.dedup();
        let input_chars = if body.prewritten.is_some() {
            0
        } else {
            system.chars().count() + prompt.chars().count()
        };
        Ok(Brief {
            id: id.to_owned(),
            kind,
            title,
            facts_digest: self.facts_digest(id)?,
            inputs: evidence.inputs(),
            system,
            prompt,
            diagrams,
            input_chars,
            prewritten: body.prewritten,
            known_paths,
        })
    }

    /// Checks what was written against the brief it came from and keeps it honest: references to
    /// files and symbols the brief did not contain become plain text, and the files it names are
    /// recorded. The result is what is stored.
    pub fn assemble(&self, brief: &Brief, markdown: &str, author: Author) -> WrittenPage {
        let keys: Vec<String> = brief.diagrams.iter().map(|d| d.key.clone()).collect();
        let known = Known {
            paths: &brief.known_paths,
            evidence: &brief.prompt,
        };
        let grounded = ground(markdown, &known, &keys);
        let hashes: HashMap<&str, &str> = brief
            .inputs
            .iter()
            .map(|i| (i.file.as_str(), i.content_hash.as_str()))
            .collect();
        let footnotes = grounded
            .cited
            .iter()
            .enumerate()
            .map(|(i, file)| Source {
                n: i as u32 + 1,
                reference: SourceRef::Lines {
                    citation: Citation {
                        file: file.clone(),
                        start_line: 1,
                        end_line: 1,
                        content_hash: hashes
                            .get(file.as_str())
                            .map(|h| (*h).to_owned())
                            .unwrap_or_default(),
                    },
                },
                excerpt: String::new(),
            })
            .collect();
        let needs_review = grounded.demoted > REVIEW_DEMOTED
            || (brief.kind != WrittenKind::Glossary && grounded.words < REVIEW_WORDS);
        WrittenPage {
            id: brief.id.clone(),
            kind: brief.kind,
            title: brief.title.clone(),
            markdown: grounded.markdown,
            footnotes,
            diagrams: grounded.diagrams,
            facts_digest: brief.facts_digest.clone(),
            inputs: brief.inputs.clone(),
            author,
            dropped: grounded.demoted,
            needs_review,
        }
    }

    /// Whether a stored page still matches the code, by the index now.
    pub fn freshness_of(&self, page: &WrittenPage) -> Result<Freshness, WikiError> {
        let snapshot = self.snapshot()?;
        let hash_of: HashMap<String, &str> = snapshot
            .imports
            .files
            .iter()
            .map(|f| (path_text(&f.path), f.content_hash.as_str()))
            .collect();
        let graph = self.pages.graph().clone();
        let current = |file: &str| {
            hash_of.get(file).map(|h| (*h).to_owned()).or_else(|| {
                let path = RootRelativePath::new(file).ok()?;
                graph
                    .read_file(&path, 512 * 1024)
                    .ok()
                    .map(|bytes| harness_workspace::repository_intelligence::content_hash(&bytes))
            })
        };
        // A page whose structural page is gone has nothing to compare: it has expired.
        let digest = self.facts_digest(&page.id).unwrap_or_default();
        Ok(freshness(
            &page.facts_digest,
            &digest,
            &page.inputs,
            &current,
        ))
    }

    /// The diagrams a page may show, drawn from the index.
    pub fn diagrams(&self, id: &str) -> Result<Vec<Diagram>, WikiError> {
        let snapshot = self.snapshot()?;
        match start_page(id) {
            Some((WrittenKind::Overview | WrittenKind::Architecture, _)) => {
                let nodes: Vec<DiagramNode> = Self::components(&snapshot)
                    .into_iter()
                    .take(MAX_COMPONENTS * 2)
                    .map(|g| DiagramNode {
                        label: g.label.clone(),
                        members: g.members.clone(),
                    })
                    .collect();
                Ok(vec![dependency_diagram(
                    &snapshot,
                    "components",
                    "How the components depend on each other",
                    &limit_nodes(nodes),
                )])
            }
            Some(_) => Ok(Vec::new()),
            None => {
                let group = Self::module_group(&snapshot, id)?;
                let nodes: Vec<DiagramNode> = folder_tree::list(&group)
                    .folders
                    .into_iter()
                    .map(|f| DiagramNode {
                        label: f.group.label.clone(),
                        members: f.group.members.clone(),
                    })
                    .collect();
                if nodes.len() < 2 {
                    return Ok(Vec::new());
                }
                Ok(vec![dependency_diagram(
                    &snapshot,
                    "structure",
                    &format!("Inside {}", group.label),
                    &limit_nodes(nodes),
                )])
            }
        }
    }
}
