//! Folder, package and repository pages: what a part of the tree contains and how it connects.
use std::collections::BTreeMap;

use harness_workspace::{
    dependency_groups::{Group, GroupKind},
    folder_tree::{self, DIRECTORY_PREFIX, REST_PREFIX},
    framework_relations::RelationKind,
    repository_intelligence::RootRelativePath,
};

use super::{absent, history, Cx, SectionBuilder, SECTION_LIMIT, SHORT_LIMIT};
use crate::{
    digest::sections_digest,
    facts::path_text,
    model::{Page, PageId, PageLevel, PageStatus, Section},
};

const MOST_DEPENDED_ON: usize = 5;

pub(super) fn page(cx: &mut Cx<'_>, id: &PageId, level: PageLevel, target: &str) -> Page {
    let group = match level {
        PageLevel::Repository => folder_tree::group_of(cx.snapshot, "dir:"),
        PageLevel::Package => cx
            .snapshot
            .groups
            .group(&harness_workspace::dependency_groups::GroupId(
                target.to_owned(),
            ))
            .cloned(),
        _ => folder_tree::group_of(cx.snapshot, target),
    };
    let Some(group) = group else {
        let status = if target.starts_with(DIRECTORY_PREFIX) || target.starts_with(REST_PREFIX) {
            PageStatus::NotIndexed
        } else {
            PageStatus::NotFound
        };
        return absent(
            id,
            level,
            cx.snapshot,
            status,
            None,
            format!("nothing is indexed at {target}."),
            Vec::new(),
        );
    };
    let files: Vec<&RootRelativePath> = group.members.iter().collect();
    let listing = folder_tree::list(&group);
    let mut sections: Vec<Section> = Vec::new();

    if level == PageLevel::Package {
        sections.extend(manifest(cx, &group));
    }
    sections.push(contents(cx, level, &group, &listing));

    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        if let Some(indexed) = cx.facts.file(file) {
            *languages
                .entry(format!("{:?}", indexed.profile).to_lowercase())
                .or_default() += 1;
        }
    }
    let mut section = SectionBuilder::new("languages", "Languages", SHORT_LIMIT);
    for (language, count) in &languages {
        section.push(format!("{language}: {count} files"), Vec::new(), Vec::new());
    }
    sections.push(section.finish());

    sections.extend(entry_points(cx, &files));
    sections.push(most_depended_on(cx, &files));
    sections.push(boundary(cx, &group));
    if level == PageLevel::Repository {
        sections.extend(repository_facts(cx));
    }
    sections.extend(history::findings(cx, &files));
    if let Some(path) =
        folder_tree::directory_path(&group).and_then(|p| RootRelativePath::new(p).ok())
    {
        sections.extend(history::folder_history(cx, &path));
    }
    sections.retain(|section| !section.facts.is_empty());

    let languages_text = languages
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let summary = format!(
        "{}: {} files ({languages_text}), {} sub-folders, {} loose files.",
        group.label,
        group.members.len(),
        listing.folders.len(),
        listing.loose.len()
    );
    let mut gaps = Vec::new();
    if cx.snapshot.imports.partial {
        gaps.push("the snapshot's import coverage is partial".to_owned());
    }
    Page {
        id: id.clone(),
        level,
        title: if level == PageLevel::Repository {
            "Repository".to_owned()
        } else {
            group.label.clone()
        },
        status: if cx.snapshot.imports.partial {
            PageStatus::Partial
        } else {
            PageStatus::Ok
        },
        language: None,
        snapshot: cx.snapshot.id.0.clone(),
        digest: sections_digest(&sections),
        summary,
        sections,
        gaps,
    }
}

fn folder_page_id(group: &Group) -> PageId {
    match group.kind {
        GroupKind::DeclaredPackage | GroupKind::SourceRoot => {
            PageId::new(PageLevel::Package, &group.id.0)
        }
        _ => PageId::folder(&group.id.0),
    }
}

fn contents(
    cx: &Cx<'_>,
    level: PageLevel,
    group: &Group,
    listing: &folder_tree::FolderListing,
) -> Section {
    let mut section = SectionBuilder::new("contents", "Contents", SECTION_LIMIT);
    if level == PageLevel::Repository {
        let (_, top) = folder_tree::top_level(cx.snapshot);
        for entry in &top {
            section.push(
                format!("{} — {} files", entry.label, entry.members.len()),
                Vec::new(),
                vec![folder_page_id(entry)],
            );
        }
        return section.finish();
    }
    let mut folders: Vec<_> = listing.folders.iter().collect();
    folders.sort_by(|a, b| {
        b.group
            .members
            .len()
            .cmp(&a.group.members.len())
            .then(a.group.id.cmp(&b.group.id))
    });
    for folder in folders {
        section.push(
            format!(
                "{} — {} files{}",
                folder.group.label,
                folder.group.members.len(),
                if folder.has_folders {
                    ", has folders"
                } else {
                    ""
                }
            ),
            Vec::new(),
            vec![PageId::folder(&folder.group.id.0)],
        );
    }
    if listing.is_leaf() {
        for file in &listing.loose {
            section.push(
                format!(
                    "{} — {} importers",
                    path_text(file),
                    cx.facts.dependents_of(file).len()
                ),
                Vec::new(),
                vec![PageId::file(&path_text(file))],
            );
        }
    } else if !listing.loose.is_empty() {
        let id = format!("{REST_PREFIX}{}", group.id.0);
        section.push(
            format!("{} loose files", listing.loose.len()),
            Vec::new(),
            vec![PageId::folder(&id)],
        );
    }
    section.finish()
}

fn manifest(cx: &mut Cx<'_>, group: &Group) -> Option<Section> {
    let package = cx.snapshot.graph.packages.iter().find(|package| {
        group.id.0.contains(&path_text(&package.manifest)) && group.id.0.ends_with(&package.name)
    })?;
    let mut section = SectionBuilder::new("manifest", "Manifest", SHORT_LIMIT);
    let hash = cx
        .snapshot
        .imports
        .configs
        .iter()
        .find(|c| c.path == package.manifest)
        .map(|c| c.content_hash.clone())
        .unwrap_or_default();
    let citation = crate::model::Citation {
        file: path_text(&package.manifest),
        start_line: 1,
        end_line: 1,
        content_hash: hash,
    };
    section.push(
        format!(
            "package {} declared in {}",
            package.name,
            path_text(&package.manifest)
        ),
        vec![citation],
        Vec::new(),
    );
    let deps = &package.facts.declared_dependencies;
    if !deps.is_empty() {
        let shown = deps.iter().take(20).cloned().collect::<Vec<_>>().join(", ");
        section.push(
            format!(
                "declares {} dependencies: {shown}{}",
                deps.len(),
                if deps.len() > 20 { ", …" } else { "" }
            ),
            Vec::new(),
            Vec::new(),
        );
    }
    let frameworks = cx
        .snapshot
        .frameworks
        .applications
        .iter()
        .filter(|a| a.package == package.id)
        .map(|a| a.framework.clone())
        .collect::<Vec<_>>();
    if !frameworks.is_empty() {
        section.push(
            format!("frameworks active: {}", frameworks.join(", ")),
            Vec::new(),
            Vec::new(),
        );
    }
    Some(section.finish())
}

fn entry_points(cx: &mut Cx<'_>, files: &[&RootRelativePath]) -> Option<Section> {
    let mut section = SectionBuilder::new("entry_points", "Entry points", SHORT_LIMIT);
    for file in files {
        let relations: Vec<_> = cx
            .facts
            .relations_of(file)
            .iter()
            .filter(|r| {
                &r.from == *file
                    && matches!(
                        r.kind,
                        RelationKind::RouteRegisteredTo | RelationKind::CommandExposedAs
                    )
            })
            .map(|r| (*r).clone())
            .collect();
        for relation in relations {
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
                    "{:?} `{}` ({}) in {}",
                    relation.kind,
                    relation.label,
                    relation.framework,
                    path_text(file)
                ),
                vec![citation],
                vec![PageId::file(&path_text(file))],
            );
        }
    }
    Some(section.finish())
}

fn most_depended_on(cx: &Cx<'_>, files: &[&RootRelativePath]) -> Section {
    let mut ranked: Vec<(&RootRelativePath, usize)> = files
        .iter()
        .map(|f| (*f, cx.facts.dependents_of(f).len()))
        .filter(|(_, n)| *n > 0)
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut section = SectionBuilder::new(
        "most_depended_on",
        "Most depended-on files",
        MOST_DEPENDED_ON,
    );
    for (file, count) in ranked {
        section.push(
            format!("{} — imported by {count} files", path_text(file)),
            Vec::new(),
            vec![PageId::file(&path_text(file))],
        );
    }
    section.finish()
}

fn boundary(cx: &Cx<'_>, group: &Group) -> Section {
    let (out, incoming) = cx.facts.crossings(group);
    let mut section = SectionBuilder::new("boundary", "Imports across its edge", SHORT_LIMIT);
    for (name, count) in out.iter().take(5) {
        section.push(
            format!("imports {count} times from {name}"),
            Vec::new(),
            Vec::new(),
        );
    }
    for (name, count) in incoming.iter().take(5) {
        section.push(
            format!("imported {count} times by {name}"),
            Vec::new(),
            Vec::new(),
        );
    }
    section.finish()
}

fn repository_facts(cx: &Cx<'_>) -> Vec<Section> {
    let mut out = Vec::new();
    let cycles = cx
        .snapshot
        .groups
        .groups
        .iter()
        .filter(|g| g.kind == GroupKind::ImportCycle)
        .count();
    let mut section = SectionBuilder::new("repository", "Repository", SHORT_LIMIT);
    section.push(
        format!(
            "{} packages declared, {} indexed files",
            cx.snapshot.graph.packages.len(),
            cx.snapshot.imports.files.len()
        ),
        Vec::new(),
        Vec::new(),
    );
    if cycles > 0 {
        section.push(format!("{cycles} import cycles"), Vec::new(), Vec::new());
    }
    let frameworks: std::collections::BTreeSet<_> = cx
        .snapshot
        .frameworks
        .applications
        .iter()
        .map(|a| a.framework.clone())
        .collect();
    if !frameworks.is_empty() {
        section.push(
            format!(
                "frameworks: {}",
                frameworks.into_iter().collect::<Vec<_>>().join(", ")
            ),
            Vec::new(),
            Vec::new(),
        );
    }
    out.push(section.finish());
    out
}
