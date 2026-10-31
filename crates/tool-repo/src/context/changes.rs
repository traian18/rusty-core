//! Mode `changes`: for a set of changed files, the packages, entry points and tests they touch,
//! the imports they make across package boundaries, and the cycles and findings they sit in.
//! There is no earlier snapshot to compare with, so imports are reported as they are now.
use std::collections::{BTreeMap, HashSet};

use rusty_repo_model::{dependency_groups::GroupKind, repository_intelligence::RootRelativePath};
use serde_json::{json, Value};

use super::{
    answer::{refresh_next, Answer, Status},
    blast::{project, reached},
    budget::Budget,
    context_next,
    facts::path_text,
    request::Section,
    support_block,
    target::{self, Absent, Target},
    tests_of::is_test_file,
    Cx,
};
use crate::failure::Failure;

pub(super) fn answer(cx: &Cx<'_>) -> Result<Answer, Failure> {
    let Resolved {
        roots,
        skipped,
        stale,
    } = resolve_targets(cx)?;
    let mut budget = Budget::new(cx.request.budget);
    let mut sections = serde_json::Map::new();

    let changed = roots.iter().map(|f| json!({"file": path_text(f), "package": cx.facts.package_of(f), "test": is_test_file(f.as_path())})).collect();
    sections.insert(
        "changed".into(),
        Value::Array(budget.fill("changed", changed)),
    );

    let reach = reach_of(cx, &roots)?;
    let packages = reach.by_package.iter().map(|(name, (changed, affected))| json!({"package": name, "changed": changed, "affected": affected})).collect();
    sections.insert(
        "packages".into(),
        Value::Array(budget.fill("packages", packages)),
    );
    if cx.request.includes(Section::Frameworks) {
        let entry_points = reach.entry_points.clone();
        sections.insert(
            "entry_points".into(),
            Value::Array(budget.fill_share("entry_points", entry_points, 4)),
        );
    }
    if cx.request.includes(Section::Tests) {
        sections.insert(
            "tests".into(),
            Value::Array(budget.fill_share("tests", reach.tests.clone(), 3)),
        );
    }
    let cross = cross_package_imports(cx, &roots);
    sections.insert(
        "cross_package_imports".into(),
        Value::Array(budget.fill("cross_package_imports", cross)),
    );
    let cycles = cycles_touching(cx, &roots);
    sections.insert("cycles".into(), Value::Array(budget.fill("cycles", cycles)));
    if let Some(findings) = cx.signals.findings_of(&roots.iter().collect::<Vec<_>>()) {
        sections.insert("findings".into(), findings);
    }
    if !skipped.is_empty() {
        sections.insert(
            "skipped_targets".into(),
            Value::Array(budget.fill("skipped_targets", skipped.clone())),
        );
    }

    let mut support = support_block(cx, None, Vec::new(), Vec::new(), None);
    if roots.is_empty() {
        support["imports"] = json!("unavailable");
    }
    Ok(Answer {
        status: status_of(cx, &roots, stale, &skipped),
        target: json!({"kind": "changes", "files": roots.len(), "requested": cx.request.targets.len()}),
        support,
        summary: summary_of(&roots, &reach, &skipped),
        sections,
        omitted: budget.omitted().clone(),
        next: next_steps(&roots, stale),
        git_snapshot: cx.signals.git_snapshot(),
        bounded: reach.bounded,
    })
}

/// The changed files that could be read, and why the other targets could not.
struct Resolved {
    roots: Vec<RootRelativePath>,
    skipped: Vec<Value>,
    stale: bool,
}

fn resolve_targets(cx: &Cx<'_>) -> Result<Resolved, Failure> {
    let mut roots: Vec<RootRelativePath> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut stale = false;
    for raw in &cx.request.targets {
        match target::resolve(&cx.context.graph, cx.snapshot, raw)? {
            Target::File { path, changed, .. } => {
                stale |= changed.is_some();
                roots.push(path);
            }
            Target::Folder(group) => skipped.push(json!({"target": raw, "status": "skipped", "reason": format!("{} is a folder; list its changed files", group.label)})),
            Target::Absent { path, why, .. } => {
                let (status, reason) = absent_reason(why);
                stale |= status == "stale";
                skipped.push(json!({"target": path, "status": status, "reason": reason}));
            }
        }
    }
    roots.sort();
    roots.dedup();
    Ok(Resolved {
        roots,
        skipped,
        stale,
    })
}

fn absent_reason(why: Absent) -> (&'static str, String) {
    match why {
        Absent::Unsupported { language } => (
            "unsupported",
            language.map_or("language is not supported".to_owned(), |l| {
                format!("{l} is not supported")
            }),
        ),
        Absent::NotIndexed { reason } => ("not_indexed", reason),
        Absent::Added => ("stale", "added since the snapshot was built".to_owned()),
        Absent::NotFound => ("not_found", "no such path".to_owned()),
    }
}

/// What the changed files reach: per package (changed, affected), tests and entry points.
struct Reach {
    by_package: BTreeMap<String, (usize, usize)>,
    affected_files: usize,
    tests: Vec<Value>,
    entry_points: Vec<Value>,
    bounded: Option<&'static str>,
}

fn reach_of(cx: &Cx<'_>, roots: &[RootRelativePath]) -> Result<Reach, Failure> {
    let mut by_package = BTreeMap::<String, (usize, usize)>::new();
    for root in roots {
        by_package
            .entry(
                cx.facts
                    .package_of(root)
                    .unwrap_or("(no package)")
                    .to_owned(),
            )
            .or_default()
            .0 += 1;
    }
    let mut reach = Reach {
        by_package,
        affected_files: 0,
        tests: Vec::new(),
        entry_points: Vec::new(),
        bounded: None,
    };
    if roots.is_empty() {
        return Ok(reach);
    }
    let radius = project(cx, roots, cx.request.depth)?;
    reach.bounded = radius.truncation;
    let root_set: HashSet<_> = roots.iter().collect();
    let reached = reached(cx, &radius, &root_set);
    reach.affected_files = reached.affected.len();
    for (package, count) in &reached.by_package {
        reach.by_package.entry(package.clone()).or_default().1 += count;
    }
    reach.tests = reached.tests;
    reach.entry_points = reached.entry_points;
    Ok(reach)
}

/// Imports of the changed files that cross a package boundary, as they are now.
fn cross_package_imports(cx: &Cx<'_>, roots: &[RootRelativePath]) -> Vec<Value> {
    let facts = &cx.facts;
    let mut cross = Vec::new();
    for root in roots {
        let from_package = facts.package_of(root);
        for edge in facts.imports_of(root) {
            let Some(target) = edge.resolution.target_file() else {
                continue;
            };
            let to_package = facts.package_of(target);
            if to_package != from_package {
                cross.push(json!({"from": path_text(root), "to": path_text(target), "from_package": from_package, "to_package": to_package}));
            }
        }
    }
    cross.dedup();
    cross
}

fn cycles_touching(cx: &Cx<'_>, roots: &[RootRelativePath]) -> Vec<Value> {
    cx.snapshot
        .groups
        .groups
        .iter()
        .filter(|g| g.kind == GroupKind::ImportCycle && roots.iter().any(|r| g.contains(r)))
        .map(|g| json!({"id": g.id.0, "label": g.label, "files": g.members.len()}))
        .collect()
}

fn summary_of(roots: &[RootRelativePath], reach: &Reach, skipped: &[Value]) -> String {
    let unread = if skipped.is_empty() {
        String::new()
    } else {
        format!("; {} targets could not be read", skipped.len())
    };
    format!(
        "{} changed files touch {} packages and may affect {} more files: {} tests, {} entry points{unread}.",
        roots.len(),
        reach.by_package.len(),
        reach.affected_files,
        reach.tests.len(),
        reach.entry_points.len(),
    )
}

fn next_steps(roots: &[RootRelativePath], stale: bool) -> Vec<Value> {
    let mut follow = Vec::new();
    if stale {
        follow.push(refresh_next(
            "some targets changed or were added since the snapshot",
        ));
    }
    if let Some(first) = roots.first() {
        follow.push(context_next(
            "blast_radius",
            &path_text(first),
            "the radius of one changed file in detail",
        ));
    }
    follow
}

fn status_of(cx: &Cx<'_>, roots: &[RootRelativePath], stale: bool, skipped: &[Value]) -> Status {
    if roots.is_empty() {
        Status::NotFound
    } else if stale {
        Status::Stale
    } else if !skipped.is_empty() || cx.snapshot.imports.partial || !cx.signals.missing.is_empty() {
        Status::Partial
    } else {
        Status::Ok
    }
}
