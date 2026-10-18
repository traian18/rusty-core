//! Mode `connections`: what the target imports, who imports it, and what is registered around it.

use harness_workspace::{
    dependency_groups::Group,
    folder_tree::{self, Folder},
    repository_intelligence::RootRelativePath,
};
use serde_json::{json, Value};

use super::{
    answer::{next, refresh_next, Answer},
    budget::Budget,
    context_next,
    facts::{path_text, Facts},
    file_target, readable_status,
    request::Section,
    support_block,
    target::Target,
    tests_of::is_test_file,
    Cx,
};
use crate::failure::Failure;

pub(super) fn answer(cx: &Cx<'_>, target: Target) -> Result<Answer, Failure> {
    Ok(match target {
        Target::File {
            path,
            indexed,
            changed,
        } => file(cx, &path, &indexed, changed),
        Target::Folder(group) => folder(cx, &group),
        Target::Absent { .. } => unreachable!("absent targets are answered before the mode runs"),
    })
}

fn file(
    cx: &Cx<'_>,
    path: &RootRelativePath,
    indexed: &harness_workspace::import_graph::IndexedFile,
    changed: Option<&'static str>,
) -> Answer {
    let facts = &cx.facts;
    let mut budget = Budget::new(cx.request.budget);
    let imports = facts.import_items(path);
    let unresolved = imports.iter().any(|item| {
        matches!(
            item["kind"].as_str(),
            Some("unresolved" | "dynamic" | "possible")
        )
    });
    let dependents = facts.dependent_files(path);
    let dependent_items: Vec<Value> = dependents
        .iter()
        .map(|(file, count)| json!({"file": path_text(file), "package": facts.package_of(file), "imports": count, "test": is_test_file(file.as_path())}))
        .collect();
    let packages: std::collections::BTreeSet<_> = dependents
        .iter()
        .filter_map(|(file, _)| facts.package_of(file))
        .collect();
    let tests: Vec<Value> = dependent_items
        .iter()
        .filter(|item| item["test"] == true)
        .map(|item| item["file"].clone())
        .collect();
    let relations = facts.relations_of(path);

    let mut sections = serde_json::Map::new();
    let (internal, external) = (
        count_kind(&imports, "internal"),
        count_kind(&imports, "external"),
    );
    let total_imports = imports.len();
    let total_dependents = dependent_items.len();
    sections.insert(
        "imports".into(),
        Value::Array(budget.fill("imports", imports)),
    );
    sections.insert(
        "dependents".into(),
        Value::Array(budget.fill("dependents", dependent_items)),
    );
    if cx.request.includes(Section::Frameworks) {
        let items = relations
            .iter()
            .map(|r| Facts::relation_item(r, path))
            .collect();
        sections.insert(
            "frameworks".into(),
            Value::Array(budget.fill("frameworks", items)),
        );
    }
    if cx.request.includes(Section::Tests) {
        sections.insert("tests".into(), Value::Array(budget.fill("tests", tests)));
    }
    if let Some(findings) = cx.signals.findings_of(&[path]) {
        sections.insert("findings".into(), findings);
    }
    if let Some(history) = cx.signals.history_of_file(path) {
        sections.insert("history".into(), history);
    }

    let language = super::support::profile_name(&indexed.profile);
    let gaps = super::support::gaps_for(&cx.capabilities, indexed.profile);
    let frameworks = facts.frameworks_for(path);
    let mut summary = format!(
        "{} imports {total_imports} things ({internal} internal, {external} external) and is imported by {total_dependents} files in {} packages",
        path_text(path),
        packages.len(),
    );
    if !relations.is_empty() {
        summary.push_str(&format!("; {} framework relations", relations.len()));
    }
    summary.push('.');
    let mut follow = Vec::new();
    if let Some(omitted) = budget.omitted().get("dependents") {
        follow.push(next(
            "repo_graph.dependents",
            json!({"file": path_text(path), "limit": 50}),
            format!("{omitted} more dependents"),
        ));
    }
    if let Some(omitted) = budget.omitted().get("imports") {
        follow.push(next(
            "repo_graph.dependencies",
            json!({"file": path_text(path), "limit": 50}),
            format!("{omitted} more imports"),
        ));
    }
    if total_dependents > 0 {
        follow.push(context_next(
            "blast_radius",
            &path_text(path),
            format!("what may break if {} changes", path_text(path)),
        ));
    }
    if changed.is_some() {
        follow.push(refresh_next("the file changed since the snapshot"));
    }
    Answer {
        status: readable_status(cx, changed, unresolved),
        target: file_target(cx, path, indexed.profile, indexed.package.as_ref()),
        support: support_block(cx, Some(language), gaps, frameworks, changed),
        summary,
        sections,
        omitted: budget.omitted().clone(),
        next: follow,
        git_snapshot: cx.signals.git_snapshot(),
        bounded: None,
    }
}

fn count_kind(items: &[Value], kind: &str) -> usize {
    items.iter().filter(|item| item["kind"] == kind).count()
}

/// Imports that cross a folder's edge, by the package (or top folder) on the other side.
pub(super) fn folder_items(folders: &[Folder]) -> Vec<Value> {
    let mut items: Vec<_> = folders.iter().collect();
    items.sort_by(|a, b| {
        b.group
            .members
            .len()
            .cmp(&a.group.members.len())
            .then(a.group.id.cmp(&b.group.id))
    });
    items
        .into_iter()
        .map(|f| json!({"id": f.group.id.0, "label": f.group.label, "files": f.group.members.len(), "has_folders": f.has_folders}))
        .collect()
}

fn pairs(items: Vec<(String, usize)>) -> Vec<Value> {
    items
        .into_iter()
        .map(|(name, imports)| json!({"name": name, "imports": imports}))
        .collect()
}

fn folder(cx: &Cx<'_>, group: &Group) -> Answer {
    let mut budget = Budget::new(cx.request.budget);
    let listing = folder_tree::list(group);
    let mut sections = serde_json::Map::new();
    let files: Vec<&RootRelativePath> = group.members.iter().collect();
    sections.insert(
        "folders".into(),
        Value::Array(budget.fill("folders", folder_items(&listing.folders))),
    );
    if listing.is_leaf() {
        let items = listing.loose.iter().map(|file| json!({"file": path_text(file), "dependents": cx.facts.dependents_of(file).len()})).collect();
        sections.insert("files".into(), Value::Array(budget.fill("files", items)));
    } else if !listing.loose.is_empty() {
        let id = format!("{}{}", folder_tree::REST_PREFIX, group.id.0);
        let sample = listing
            .loose
            .iter()
            .take(5)
            .map(path_text)
            .collect::<Vec<_>>();
        sections.insert(
            "loose_files".into(),
            json!({"id": id, "files": listing.loose.len(), "sample": sample}),
        );
    }
    let (out, incoming) = cx.facts.crossings(group);
    sections.insert(
        "imports_out".into(),
        Value::Array(budget.fill("imports_out", pairs(out))),
    );
    sections.insert(
        "imported_by".into(),
        Value::Array(budget.fill("imported_by", pairs(incoming))),
    );
    if cx.request.includes(Section::Frameworks) {
        let mut items = Vec::new();
        for file in &files {
            for relation in cx
                .facts
                .relations_of(file)
                .iter()
                .filter(|r| &r.from == *file)
            {
                items.push(Facts::relation_item(relation, file));
            }
        }
        sections.insert(
            "frameworks".into(),
            Value::Array(budget.fill("frameworks", items)),
        );
    }
    if let Some(findings) = cx.signals.findings_of(&files) {
        sections.insert("findings".into(), findings);
    }
    let folder_path =
        folder_tree::directory_path(group).and_then(|p| RootRelativePath::new(p).ok());
    if let Some(history) = folder_path
        .as_ref()
        .and_then(|p| cx.signals.history_of_folder(p))
    {
        sections.insert("history".into(), history);
    }
    let summary = format!(
        "{} holds {} files in {} sub-folders and {} loose files.",
        group.label,
        group.members.len(),
        listing.folders.len(),
        listing.loose.len()
    );
    let mut follow = vec![context_next(
        "orient",
        &group.id.0,
        "a map of what is inside",
    )];
    if !group.members.is_empty() {
        follow.push(context_next(
            "blast_radius",
            &group.id.0,
            "what may break if this folder changes",
        ));
    }
    Answer {
        status: readable_status(cx, None, false),
        target: json!({"kind": "folder", "id": group.id.0, "label": group.label, "files": group.members.len()}),
        support: support_block(cx, None, Vec::new(), Vec::new(), None),
        summary,
        sections,
        omitted: budget.omitted().clone(),
        next: follow,
        git_snapshot: cx.signals.git_snapshot(),
        bounded: None,
    }
}
