//! File pages: what the file is, imports, importers, framework links, tests, findings, history.
use std::collections::BTreeMap;

use harness_workspace::{
    import_graph::{ByteSpan, IndexedFile, Resolution},
    repository_intelligence::{
        ignored_directory, known_unsupported_language, PathState, RootRelativePath,
    },
};

use super::{absent, history, Cx, SectionBuilder, SECTION_LIMIT, SHORT_LIMIT};
use crate::{
    digest::sections_digest,
    facts::path_text,
    model::{Page, PageId, PageLevel, PageStatus, Section},
    tests_of::is_test_file,
};

pub(super) fn page(cx: &mut Cx<'_>, id: &PageId, target: &str) -> Page {
    let Ok(path) = RootRelativePath::new(target.trim_end_matches('/')) else {
        return absent(
            id,
            PageLevel::File,
            cx.snapshot,
            PageStatus::NotFound,
            None,
            format!("{target} is not a workspace-relative path."),
            Vec::new(),
        );
    };
    let Some(indexed) = cx.facts.file(&path).cloned() else {
        return unindexed(cx, id, &path);
    };
    let changed = match cx.graph.path_state(&path) {
        PathState::File { content_hash } if content_hash != indexed.content_hash => {
            Some("content changed since the snapshot")
        }
        PathState::Missing => Some("deleted since the snapshot"),
        _ => None,
    };
    let mut sections: Vec<Section> = vec![imports(cx, &path, &indexed)];
    let importers = importers(cx, &path);
    let dependents = cx.facts.dependent_files(&path);
    sections.push(importers);
    if let Some(section) = frameworks(cx, &path) {
        sections.push(section);
    }
    let tests: Vec<_> = dependents
        .iter()
        .filter(|(file, _)| is_test_file(file.as_path()))
        .collect();
    if !tests.is_empty() {
        let mut section = SectionBuilder::new("tests", "Tests that reach it", SHORT_LIMIT);
        for (file, _) in tests {
            section.push(
                format!("{} imports it", path_text(file)),
                Vec::new(),
                vec![PageId::file(&path_text(file))],
            );
        }
        sections.push(section.finish());
    }
    sections.extend(history::findings(cx, &[&path]));
    sections.extend(history::file_history(cx, &path));
    sections.retain(|section| !section.facts.is_empty());

    let language = format!("{:?}", indexed.profile).to_lowercase();
    let unresolved = cx.facts.imports_of(&path).iter().any(|edge| {
        !matches!(
            edge.resolution,
            Resolution::ResolvedInternal { .. } | Resolution::DeclaredExternal { .. }
        )
    });
    let mut gaps = Vec::new();
    if cx.snapshot.imports.partial {
        gaps.push("the snapshot's import coverage is partial".to_owned());
    }
    if unresolved {
        gaps.push("some imports are unresolved, possible or dynamic".to_owned());
    }
    if let Some(change) = changed {
        gaps.push(change.to_owned());
    }
    let status = if changed.is_some() {
        PageStatus::Stale
    } else if cx.snapshot.imports.partial || unresolved {
        PageStatus::Partial
    } else {
        PageStatus::Ok
    };
    let package = cx.facts.package_of(&path).unwrap_or("(no package)");
    let summary = format!(
        "{} is a {language} file in {package}; it imports {} things and is imported by {} files.",
        path_text(&path),
        cx.facts.imports_of(&path).len(),
        dependents.len()
    );
    Page {
        id: id.clone(),
        level: PageLevel::File,
        title: path_text(&path),
        status,
        language: Some(language),
        snapshot: cx.snapshot.id.0.clone(),
        digest: sections_digest(&sections),
        summary,
        sections,
        gaps,
    }
}

fn unindexed(cx: &Cx<'_>, id: &PageId, path: &RootRelativePath) -> Page {
    let text = path_text(path);
    let snapshot = cx.snapshot;
    match cx.graph.path_state(path) {
        PathState::Missing => absent(
            id,
            PageLevel::File,
            snapshot,
            PageStatus::NotFound,
            None,
            format!("nothing exists at {text}."),
            Vec::new(),
        ),
        PathState::Directory => absent(
            id,
            PageLevel::File,
            snapshot,
            PageStatus::NotIndexed,
            None,
            format!("{text} is a folder; ask for folder:dir:{text}."),
            Vec::new(),
        ),
        _ => {
            if let Some(ignored) = ignored_directory(path.as_path()) {
                return absent(
                    id,
                    PageLevel::File,
                    snapshot,
                    PageStatus::NotIndexed,
                    None,
                    format!("{text} is under the ignored directory `{ignored}`."),
                    Vec::new(),
                );
            }
            match cx.graph.classify(path) {
                Some(profile) => absent(
                    id,
                    PageLevel::File,
                    snapshot,
                    PageStatus::Stale,
                    Some(format!("{profile:?}").to_lowercase()),
                    format!("{text} was added after the snapshot was built."),
                    vec!["refresh the repository to index it".to_owned()],
                ),
                None => {
                    let language = known_unsupported_language(path.as_path());
                    absent(
                        id,
                        PageLevel::File,
                        snapshot,
                        PageStatus::Unsupported,
                        language.map(str::to_owned),
                        format!(
                            "{} is not supported, so {text} has no page; read the file instead.",
                            language.unwrap_or("this file type")
                        ),
                        Vec::new(),
                    )
                }
            }
        }
    }
}

/// One distinct thing a file imports, with the first site that does it.
struct ImportItem {
    kind: &'static str,
    name: String,
    count: usize,
    span: ByteSpan,
    target: Option<RootRelativePath>,
}

fn imports(cx: &mut Cx<'_>, path: &RootRelativePath, indexed: &IndexedFile) -> Section {
    let mut items: BTreeMap<(usize, String), ImportItem> = BTreeMap::new();
    for edge in cx.facts.imports_of(path) {
        let (order, kind, name, target) = match &edge.resolution {
            Resolution::ResolvedInternal { target_file, .. } => (
                0,
                "imports",
                path_text(target_file),
                Some(target_file.clone()),
            ),
            Resolution::DeclaredExternal { package } => (1, "uses external", package.clone(), None),
            Resolution::Possible { .. } => {
                (2, "possibly imports", edge.fact.spelling.clone(), None)
            }
            Resolution::Unresolved { .. } => {
                (3, "cannot resolve", edge.fact.spelling.clone(), None)
            }
            Resolution::DynamicOrUnsupported { .. } => {
                (4, "dynamic import of", edge.fact.spelling.clone(), None)
            }
        };
        items
            .entry((order, name.clone()))
            .and_modify(|item| item.count += 1)
            .or_insert(ImportItem {
                kind,
                name,
                count: 1,
                span: edge.fact.span,
                target,
            });
    }
    let mut section = SectionBuilder::new("imports", "Imports", SECTION_LIMIT);
    for item in items.into_values() {
        let count = if item.count > 1 {
            format!(" ({} imports)", item.count)
        } else {
            String::new()
        };
        let citation = cx.sources.cite(path, item.span, &indexed.content_hash);
        let refs = item
            .target
            .map(|target| vec![PageId::file(&path_text(&target))])
            .unwrap_or_default();
        section.push(
            format!("{} {}{count}", item.kind, item.name),
            vec![citation],
            refs,
        );
    }
    section.finish()
}

fn importers(cx: &mut Cx<'_>, path: &RootRelativePath) -> Section {
    let mut section = SectionBuilder::new("imported_by", "Imported by", SECTION_LIMIT);
    for (file, count) in cx.facts.dependent_files(path) {
        let first = cx
            .facts
            .imports_of(&file)
            .iter()
            .find(|edge| edge.resolution.target_file() == Some(path))
            .map(|edge| edge.fact.span);
        let hash = cx
            .facts
            .file(&file)
            .map(|f| f.content_hash.clone())
            .unwrap_or_default();
        let citations = first
            .map(|span| vec![cx.sources.cite(&file, span, &hash)])
            .unwrap_or_default();
        section.push(
            format!(
                "{}{}",
                path_text(&file),
                if count > 1 {
                    format!(" ({count} imports)")
                } else {
                    String::new()
                }
            ),
            citations,
            vec![PageId::file(&path_text(&file))],
        );
    }
    section.finish()
}

fn frameworks(cx: &mut Cx<'_>, path: &RootRelativePath) -> Option<Section> {
    let relations: Vec<_> = cx.facts.relations_of(path).to_vec();
    if relations.is_empty() {
        return None;
    }
    let mut section = SectionBuilder::new("frameworks", "Framework links", SECTION_LIMIT);
    for relation in relations {
        let role = if &relation.from == path {
            "registers"
        } else {
            "receives"
        };
        let other = if &relation.from == path {
            relation.to.as_ref().map(path_text)
        } else {
            Some(path_text(&relation.from))
        };
        let hash = cx
            .facts
            .file(&relation.evidence.file)
            .map(|f| f.content_hash.clone())
            .unwrap_or_default();
        let citation = cx
            .sources
            .cite(&relation.evidence.file, relation.evidence.span, &hash);
        section.push(
            format!(
                "{role} {:?} `{}` ({}, {:?}){}",
                relation.kind,
                relation.label,
                relation.framework,
                relation.certainty,
                other
                    .as_ref()
                    .map(|o| format!(" with {o}"))
                    .unwrap_or_default()
            ),
            vec![citation],
            other.map(|o| vec![PageId::file(&o)]).unwrap_or_default(),
        );
    }
    Some(section.finish())
}
