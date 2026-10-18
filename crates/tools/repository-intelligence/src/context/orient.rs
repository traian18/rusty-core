//! Mode `orient`: a short map of a folder, read the same way the architecture canvas reads it: the
//! sub-folders, one entry for the loose files, and the few things worth looking at first.
use std::collections::BTreeMap;

use harness_workspace::{
    dependency_groups::Group, folder_tree, framework_relations::RelationKind,
    repository_intelligence::RootRelativePath,
};
use serde_json::{json, Value};

use super::{
    answer::Answer,
    budget::Budget,
    connections::{self, folder_items},
    context_next,
    facts::path_text,
    readable_status,
    request::Section,
    support_block,
    target::Target,
    Cx,
};
use crate::failure::Failure;

const MOST_DEPENDED_ON: usize = 5;
const SAMPLE_FILES: usize = 5;

pub(super) fn answer(cx: &Cx<'_>, target: Target) -> Result<Answer, Failure> {
    let group = match target {
        Target::Folder(group) => group,
        // A file is oriented through the folder it lives in.
        Target::File { path, .. } => {
            let parent = path
                .as_path()
                .parent()
                .map(|p| format!("{}{}", folder_tree::DIRECTORY_PREFIX, p.display()));
            match parent.and_then(|id| folder_tree::group_of(cx.snapshot, &id)) {
                Some(group) => group,
                None => {
                    return connections::answer(
                        cx,
                        Target::File {
                            path: path.clone(),
                            indexed: cx
                                .facts
                                .file(&path)
                                .cloned()
                                .ok_or_else(|| Failure::invalid("file is not indexed"))?,
                            changed: None,
                        },
                    )
                }
            }
        }
        Target::Absent { .. } => unreachable!("absent targets are answered before the mode runs"),
    };
    Ok(folder(cx, &group))
}

fn folder(cx: &Cx<'_>, group: &Group) -> Answer {
    let listing = folder_tree::list(group);
    let files: Vec<&RootRelativePath> = group.members.iter().collect();
    let mut budget = Budget::new(cx.request.budget);
    let mut sections = serde_json::Map::new();

    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        if let Some(indexed) = cx.facts.file(file) {
            *languages
                .entry(super::support::profile_name(&indexed.profile))
                .or_default() += 1;
        }
    }
    sections.insert("languages".into(), json!(languages));
    sections.insert(
        "folders".into(),
        Value::Array(budget.fill("folders", folder_items(&listing.folders))),
    );
    if listing.is_leaf() {
        let items = listing.loose.iter().map(|file| json!({"file": path_text(file), "dependents": cx.facts.dependents_of(file).len()})).collect();
        sections.insert("files".into(), Value::Array(budget.fill("files", items)));
    } else if !listing.loose.is_empty() {
        let id = format!("{}{}", folder_tree::REST_PREFIX, group.id.0);
        sections.insert("loose_files".into(), json!({"id": id, "files": listing.loose.len(), "sample": listing.loose.iter().take(SAMPLE_FILES).map(path_text).collect::<Vec<_>>()}));
    }

    let mut entry_points = Vec::new();
    for file in &files {
        for relation in cx.facts.relations_of(file).iter().filter(|r| {
            &r.from == *file
                && matches!(
                    r.kind,
                    RelationKind::RouteRegisteredTo | RelationKind::CommandExposedAs
                )
        }) {
            entry_points.push(json!({"file": path_text(file), "kind": relation.kind, "framework": relation.framework, "label": relation.label}));
        }
    }
    if cx.request.includes(Section::Frameworks) {
        sections.insert(
            "entry_points".into(),
            Value::Array(budget.fill("entry_points", entry_points.clone())),
        );
    }

    let mut depended: Vec<(&RootRelativePath, usize)> = files
        .iter()
        .map(|f| (*f, cx.facts.dependents_of(f).len()))
        .filter(|(_, n)| *n > 0)
        .collect();
    depended.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top = depended
        .iter()
        .take(MOST_DEPENDED_ON)
        .map(|(file, n)| json!({"file": path_text(file), "dependents": n}))
        .collect();
    sections.insert(
        "most_depended_on".into(),
        Value::Array(budget.fill("most_depended_on", top)),
    );
    if let Some(findings) = cx.signals.findings_of(&files) {
        sections.insert("findings".into(), findings);
    }
    if let Some(history) = folder_tree::directory_path(group)
        .and_then(|p| RootRelativePath::new(p).ok())
        .and_then(|p| cx.signals.history_of_folder(&p))
    {
        sections.insert("history".into(), history);
    }

    let summary = format!(
        "{}: {} files ({}), {} sub-folders, {} loose files, {} entry points.",
        group.label,
        group.members.len(),
        languages
            .iter()
            .map(|(name, n)| format!("{name} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
        listing.folders.len(),
        listing.loose.len(),
        entry_points.len(),
    );
    let mut follow = Vec::new();
    if let Some(biggest) = folder_items(&listing.folders).first() {
        follow.push(context_next(
            "orient",
            biggest["id"].as_str().unwrap_or_default(),
            format!(
                "open the largest sub-folder ({})",
                biggest["label"].as_str().unwrap_or_default()
            ),
        ));
    }
    if let Some((file, n)) = depended.first() {
        follow.push(context_next(
            "connections",
            &path_text(file),
            format!("the most depended-on file ({n} dependents)"),
        ));
    }
    Answer {
        status: readable_status(cx, None, false),
        target: json!({"kind": "folder", "id": group.id.0, "label": group.label, "files": group.members.len(), "leaf": listing.is_leaf()}),
        support: support_block(cx, None, Vec::new(), Vec::new(), None),
        summary,
        sections,
        omitted: budget.omitted().clone(),
        next: follow,
        git_snapshot: cx.signals.git_snapshot(),
        bounded: None,
    }
}
