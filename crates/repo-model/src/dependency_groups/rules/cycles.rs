//! Import cycles: strongly connected components of the internal import graph.
use std::collections::{BTreeSet, HashMap};

use crate::{
    dependency_groups::{
        analyzer::{GroupInput, GroupRule},
        model::{Group, GroupBasis, GroupId, GroupKind},
        scc::strongly_connected_components,
    },
    import_graph::{ImportEdge, ImportKind},
    repository_intelligence::RootRelativePath,
};

/// Reports files that are mutually reachable through resolved internal imports.
///
/// Runtime imports are examined first. A component that only appears once type-only
/// imports are counted is reported separately and flagged, because such a cycle cannot
/// fail at load time. Nothing here claims the cycle was intended or is a defect.
pub struct CycleRule;

impl GroupRule for CycleRule {
    fn id(&self) -> &'static str {
        "import-cycle"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> GroupKind {
        GroupKind::ImportCycle
    }

    fn groups(&self, input: &GroupInput<'_>) -> Vec<Group> {
        let files = input
            .imports
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let positions = files
            .iter()
            .enumerate()
            .map(|(position, path)| (path, position as u32))
            .collect::<HashMap<_, _>>();
        let runtime = components(input, &files, &positions, true);
        let runtime_sets = runtime.iter().cloned().collect::<BTreeSet<_>>();
        let all = components(input, &files, &positions, false);
        let mut groups = Vec::new();
        for (members, type_only) in runtime.into_iter().map(|members| (members, false)).chain(
            all.into_iter()
                .filter(|members| !runtime_sets.contains(members))
                .map(|members| (members, true)),
        ) {
            groups.push(cycle_group(input, &files, &positions, members, type_only));
        }
        groups
    }
}

/// Components with at least two files, as sorted file-position lists.
fn components(
    input: &GroupInput<'_>,
    files: &[RootRelativePath],
    positions: &HashMap<&RootRelativePath, u32>,
    runtime_only: bool,
) -> Vec<Vec<u32>> {
    let mut adjacency = vec![Vec::new(); files.len()];
    for edge in internal_edges(input, runtime_only) {
        let Some(target) = edge.resolution.target_file() else {
            continue;
        };
        if let (Some(&from), Some(&to)) = (
            positions.get(&edge.fact.importing_file),
            positions.get(target),
        ) {
            if from != to {
                adjacency[from as usize].push(to);
            }
        }
    }
    for successors in &mut adjacency {
        successors.sort_unstable();
        successors.dedup();
    }
    strongly_connected_components(&adjacency)
        .into_iter()
        .filter(|component| component.len() > 1)
        .collect()
}

fn internal_edges<'a>(
    input: &'a GroupInput<'_>,
    runtime_only: bool,
) -> impl Iterator<Item = &'a ImportEdge> {
    input.imports.edges.iter().filter(move |edge| {
        edge.resolution.target_file().is_some()
            && (!runtime_only || edge.fact.kind == ImportKind::Runtime)
    })
}

fn cycle_group(
    input: &GroupInput<'_>,
    files: &[RootRelativePath],
    positions: &HashMap<&RootRelativePath, u32>,
    component: Vec<u32>,
    involves_type_only: bool,
) -> Group {
    let members = component
        .iter()
        .map(|&position| files[position as usize].clone())
        .collect::<Vec<_>>();
    let inside = component.iter().copied().collect::<BTreeSet<_>>();
    let mut sites = internal_edges(input, !involves_type_only)
        .filter(|edge| {
            let from = positions.get(&edge.fact.importing_file);
            let to = edge.resolution.target_file().and_then(|t| positions.get(t));
            matches!((from, to), (Some(from), Some(to)) if from != to && inside.contains(from) && inside.contains(to))
        })
        .map(|edge| edge.fact.id.clone())
        .collect::<Vec<_>>();
    let evidence_truncated = sites.len() > input.limits.max_cycle_evidence;
    sites.truncate(input.limits.max_cycle_evidence);
    let first = members[0].as_path().display().to_string();
    let suffix = if involves_type_only { "+types" } else { "" };
    Group {
        id: GroupId(format!("import_cycle:{first}{suffix}")),
        kind: GroupKind::ImportCycle,
        label: format!("{} files, starting at {first}", members.len()),
        basis: GroupBasis::Derived,
        explanation: if involves_type_only {
            "These files are mutually reachable through resolved imports only when type-only imports are counted; the cycle is erased at runtime.".to_owned()
        } else {
            "These files are mutually reachable through resolved runtime imports. This states reachability, not intent.".to_owned()
        },
        evidence: Vec::new(),
        cycle_sites: sites,
        evidence_truncated,
        involves_type_only,
        members,
    }
}
