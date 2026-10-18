//! Mode `blast_radius`: what may break if the target changes, through resolved internal imports,
//! with the framework entry points and tests it reaches. Import edges only; co-change hints are a
//! separate, labelled section.
use std::collections::{BTreeMap, HashSet};

use harness_workspace::{
    dependency_groups::{GraphDirection, GraphNode, GraphQuery},
    framework_relations::RelationKind,
    import_graph::ImportFilter,
    repository_intelligence::RootRelativePath,
};
use serde_json::{json, Value};

use super::{
    answer::{next, refresh_next, Answer},
    budget::Budget,
    context_next,
    facts::path_text,
    file_target, readable_status,
    request::{Section, WIDE_DEPTH},
    support_block,
    target::Target,
    tests_of::is_test_file,
    Cx,
};
use crate::{failure::Failure, graph::truncation_name};

/// Most files used as roots when the target is a whole folder.
const MAX_ROOTS: usize = 500;
const NODE_LIMIT: usize = 500;
const EDGE_LIMIT: usize = 2_000;

/// Everything a change to `roots` reaches.
pub(super) struct Radius {
    pub nodes: Vec<GraphNode>,
    pub truncation: Option<&'static str>,
}

pub(super) fn project(
    cx: &Cx<'_>,
    roots: &[RootRelativePath],
    depth: usize,
) -> Result<Radius, Failure> {
    let projection = cx.context.graph.graph(&GraphQuery {
        snapshot: cx.snapshot.id.clone(),
        roots: roots.to_vec(),
        direction: GraphDirection::Dependents,
        depth,
        node_limit: NODE_LIMIT,
        edge_limit: EDGE_LIMIT,
        filter: ImportFilter::default(),
    })?;
    Ok(Radius {
        nodes: projection.nodes,
        truncation: truncation_name(projection.truncation),
    })
}

/// What a radius contains, ranked: depth first, then crossing a package boundary, then how many
/// files depend on it, then path.
pub(super) struct Reached {
    pub dependents: Vec<Value>,
    pub tests: Vec<Value>,
    pub entry_points: Vec<Value>,
    pub by_depth: BTreeMap<usize, usize>,
    pub by_package: BTreeMap<String, usize>,
    pub affected: Vec<RootRelativePath>,
}

pub(super) fn reached(cx: &Cx<'_>, radius: &Radius, roots: &HashSet<&RootRelativePath>) -> Reached {
    let facts = &cx.facts;
    let root_packages: HashSet<_> = roots
        .iter()
        .filter_map(|root| facts.package_of(root))
        .collect();
    let mut nodes: Vec<&GraphNode> = radius
        .nodes
        .iter()
        .filter(|n| n.depth > 0 && !roots.contains(&n.file))
        .collect();
    let crosses = |node: &GraphNode| {
        !facts
            .package_of(&node.file)
            .is_some_and(|p| root_packages.contains(p))
    };
    nodes.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(crosses(b).cmp(&crosses(a)))
            .then(
                facts
                    .dependents_of(&b.file)
                    .len()
                    .cmp(&facts.dependents_of(&a.file).len()),
            )
            .then(a.file.cmp(&b.file))
    });
    let mut out = Reached {
        dependents: Vec::new(),
        tests: Vec::new(),
        entry_points: Vec::new(),
        by_depth: BTreeMap::new(),
        by_package: BTreeMap::new(),
        affected: Vec::new(),
    };
    let mut seen_relations = HashSet::new();
    let touching = |file: &RootRelativePath,
                    depth: usize,
                    out: &mut Reached,
                    seen: &mut HashSet<String>| {
        for relation in facts.relations_of(file) {
            let entry = matches!(
                relation.kind,
                RelationKind::RouteRegisteredTo | RelationKind::CommandExposedAs
            );
            if entry && relation.from == *file && seen.insert(relation.id.0.clone()) {
                out.entry_points.push(json!({"file": path_text(file), "depth": depth, "kind": relation.kind, "framework": relation.framework, "label": relation.label}));
            }
        }
    };
    for root in roots {
        touching(root, 0, &mut out, &mut seen_relations);
    }
    for node in nodes {
        *out.by_depth.entry(node.depth).or_default() += 1;
        *out.by_package
            .entry(
                facts
                    .package_of(&node.file)
                    .unwrap_or("(no package)")
                    .to_owned(),
            )
            .or_default() += 1;
        out.affected.push(node.file.clone());
        touching(&node.file, node.depth, &mut out, &mut seen_relations);
        let item = json!({"file": path_text(&node.file), "depth": node.depth, "package": facts.package_of(&node.file), "crosses_package": crosses(node)});
        if is_test_file(node.file.as_path()) {
            out.tests.push(item);
        } else {
            out.dependents.push(item);
        }
    }
    out
}

pub(super) fn answer(cx: &Cx<'_>, target: Target) -> Result<Answer, Failure> {
    let (roots, label, target_block, changed, profile) = match &target {
        Target::File {
            path,
            indexed,
            changed,
        } => (
            vec![path.clone()],
            path_text(path),
            file_target(cx, path, indexed.profile, indexed.package.as_ref()),
            *changed,
            Some(indexed.profile),
        ),
        Target::Folder(group) => (
            group.members.iter().take(MAX_ROOTS).cloned().collect(),
            group.label.clone(),
            json!({"kind": "folder", "id": group.id.0, "label": group.label, "files": group.members.len()}),
            None,
            None,
        ),
        Target::Absent { .. } => unreachable!("absent targets are answered before the mode runs"),
    };
    let wide = matches!(target, Target::Folder(_));
    let depth = if wide {
        cx.request.depth.min(WIDE_DEPTH)
    } else {
        cx.request.depth
    };
    let radius = project(cx, &roots, depth)?;
    let root_set: HashSet<_> = roots.iter().collect();
    let reached = reached(cx, &radius, &root_set);

    let mut budget = Budget::new(cx.request.budget);
    let mut sections = serde_json::Map::new();
    sections.insert("affected".into(), json!({"files": reached.affected.len(), "by_depth": reached.by_depth, "by_package": reached.by_package}));
    if cx.request.includes(Section::Frameworks) {
        sections.insert(
            "entry_points".into(),
            Value::Array(budget.fill_share("entry_points", reached.entry_points.clone(), 4)),
        );
    }
    if cx.request.includes(Section::Tests) {
        sections.insert(
            "tests".into(),
            Value::Array(budget.fill_share("tests", reached.tests.clone(), 3)),
        );
    }
    sections.insert(
        "dependents".into(),
        Value::Array(budget.fill("dependents", reached.dependents.clone())),
    );
    let mut scope: Vec<&RootRelativePath> = roots.iter().collect();
    scope.extend(reached.affected.iter());
    if let Some(findings) = cx.signals.findings_of(&scope) {
        sections.insert("findings".into(), findings);
    }
    if let Some(hints) = cx
        .signals
        .co_change_hints(&roots.iter().collect::<Vec<_>>())
    {
        sections.insert("co_change_hints".into(), hints);
    }

    let packages = reached.by_package.len();
    let summary = format!(
        "Changing {label} may affect {} files in {packages} packages (depth {depth}): {} tests, {} entry points.",
        reached.affected.len(),
        reached.tests.len(),
        reached.entry_points.len(),
    );
    let mut follow = Vec::new();
    if let Some(reason) = radius.truncation {
        follow.push(next("repo_graph.impact", json!({"files": roots.iter().take(64).map(path_text).collect::<Vec<_>>(), "depth": cx.request.depth, "node_limit": 1000}), format!("the radius was cut by {reason}")));
    }
    if changed.is_some() {
        follow.push(refresh_next("the file changed since the snapshot"));
    }
    if let Target::File { path, .. } = &target {
        follow.push(context_next(
            "connections",
            &path_text(path),
            "the direct imports and importers",
        ));
    }
    let language = profile.map(|p| super::support::profile_name(&p));
    let gaps = profile
        .map(|p| super::support::gaps_for(&cx.capabilities, p))
        .unwrap_or_default();
    let frameworks = roots
        .first()
        .map(|r| cx.facts.frameworks_for(r))
        .unwrap_or_default();
    Ok(Answer {
        status: readable_status(cx, changed, false),
        target: target_block,
        support: support_block(cx, language, gaps, frameworks, changed),
        summary,
        sections,
        omitted: budget.omitted().clone(),
        next: follow,
        git_snapshot: cx.signals.git_snapshot(),
        bounded: radius.truncation,
    })
}
