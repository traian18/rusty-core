//! Bounded query and traversal over one immutable snapshot generation.

use std::collections::{BTreeSet, VecDeque};

use crate::api::CallGraphError;
use crate::model::*;
use crate::snapshot_repository::GraphSnapshot;

pub(crate) const PAGE_LIMIT: u32 = 500;
pub(crate) const DEPTH_LIMIT: u32 = 32;
/// Maximum nodes expanded by one traversal before it stops and reports the budget.
pub(crate) const VISIT_LIMIT: usize = 10_000;
const ROOT_LIMIT: usize = 64;

pub(crate) fn metadata(snapshot: &GraphSnapshot) -> QueryMetadata {
    QueryMetadata {
        snapshot: snapshot.id.clone(),
        partial: snapshot.partial,
        diagnostics: snapshot.diagnostics.clone(),
        truncated: false,
        limits: snapshot.limits.clone(),
        freshness: snapshot.freshness.clone(),
        generated_at_unix: snapshot.generated_at_unix,
    }
}

pub(crate) fn validate_limit(limit: u32) -> Result<(), CallGraphError> {
    if limit == 0 {
        return Err(CallGraphError::InvalidRequest(
            "limit must be at least 1".into(),
        ));
    }
    Ok(())
}

fn known_callable(snapshot: &GraphSnapshot, id: &SymbolId) -> Result<(), CallGraphError> {
    if snapshot.index.callable_by_id.contains_key(id) {
        Ok(())
    } else {
        Err(CallGraphError::InvalidRequest(format!(
            "unknown callable `{}` in snapshot {}",
            id.0, snapshot.id
        )))
    }
}

fn allows(filter: &[Resolution], resolution: Resolution, default_all: bool) -> bool {
    if filter.is_empty() {
        default_all || resolution != Resolution::Unresolved
    } else {
        filter.contains(&resolution)
    }
}

pub(crate) fn find_callables(
    snapshot: &GraphSnapshot,
    request: &FindCallablesRequest,
) -> Result<CallableResponse, CallGraphError> {
    validate_limit(request.limit)?;
    let items = snapshot
        .callables
        .iter()
        .filter(|item| {
            request
                .language
                .as_ref()
                .is_none_or(|language| language == &item.language)
                && request
                    .path
                    .as_ref()
                    .is_none_or(|path| path == &item.span.path)
                && request.name.as_ref().is_none_or(|name| {
                    item.name == *name
                        || item.qualified_name == *name
                        || item.qualified_name.ends_with(&format!(".{name}"))
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(CallableResponse {
        page: page(items, request.cursor.clone(), request.limit, snapshot)?,
    })
}

pub(crate) fn calls_from(
    snapshot: &GraphSnapshot,
    request: &CallsFromRequest,
) -> Result<CallsResponse, CallGraphError> {
    validate_limit(request.limit)?;
    known_callable(snapshot, &request.caller)?;
    let items = snapshot
        .index
        .by_caller
        .get(&request.caller)
        .into_iter()
        .flatten()
        .map(|position| &snapshot.calls[*position])
        .filter(|call| allows(&request.resolutions, call.resolution, true))
        .cloned()
        .collect();
    Ok(CallsResponse {
        page: page(items, request.cursor.clone(), request.limit, snapshot)?,
    })
}

pub(crate) fn calls_to(
    snapshot: &GraphSnapshot,
    request: &CallsToRequest,
) -> Result<CallsResponse, CallGraphError> {
    validate_limit(request.limit)?;
    known_callable(snapshot, &request.target)?;
    let items = snapshot
        .index
        .by_target
        .get(&request.target)
        .into_iter()
        .flatten()
        .map(|position| &snapshot.calls[*position])
        .filter(|call| allows(&request.resolutions, call.resolution, false))
        .cloned()
        .collect();
    Ok(CallsResponse {
        page: page(items, request.cursor.clone(), request.limit, snapshot)?,
    })
}

pub(crate) fn traverse(
    snapshot: &GraphSnapshot,
    request: &TraversalRequest,
) -> Result<TraversalResponse, CallGraphError> {
    validate_limit(request.limit)?;
    if request.depth > DEPTH_LIMIT {
        return Err(CallGraphError::LimitExceeded(format!(
            "traversal depth {} exceeds the maximum of {DEPTH_LIMIT}",
            request.depth
        )));
    }
    if request.roots.is_empty() {
        return Err(CallGraphError::InvalidRequest(
            "at least one root callable is required".into(),
        ));
    }
    if request.roots.len() > ROOT_LIMIT {
        return Err(CallGraphError::LimitExceeded(format!(
            "at most {ROOT_LIMIT} roots are accepted"
        )));
    }
    for root in &request.roots {
        known_callable(snapshot, root)?;
    }
    let mut seen: BTreeSet<SymbolId> = BTreeSet::new();
    let mut edge_positions: BTreeSet<usize> = BTreeSet::new();
    let mut queue: VecDeque<(SymbolId, u32)> = request
        .roots
        .iter()
        .cloned()
        .map(|root| (root, 0))
        .collect();
    let mut depth_cut = false;
    let mut visit_cut = false;
    let mut expanded = 0usize;
    let follow_callees = matches!(request.direction, Direction::Callees | Direction::Both);
    let follow_callers = matches!(request.direction, Direction::Callers | Direction::Both);
    while let Some((id, depth)) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if expanded >= VISIT_LIMIT {
            visit_cut = true;
            break;
        }
        expanded += 1;
        let mut neighbours: Vec<SymbolId> = Vec::new();
        if follow_callees {
            for position in snapshot.index.by_caller.get(&id).into_iter().flatten() {
                let call = &snapshot.calls[*position];
                if !allows(&request.resolutions, call.resolution, true) {
                    continue;
                }
                edge_positions.insert(*position);
                neighbours.extend(call.target.iter().chain(call.candidates.iter()).cloned());
            }
        }
        if follow_callers {
            for position in snapshot.index.by_target.get(&id).into_iter().flatten() {
                let call = &snapshot.calls[*position];
                if !allows(&request.resolutions, call.resolution, false) {
                    continue;
                }
                edge_positions.insert(*position);
                neighbours.push(call.caller.clone());
            }
        }
        neighbours.retain(|neighbour| snapshot.index.callable_by_id.contains_key(neighbour));
        if depth >= request.depth {
            if neighbours.iter().any(|neighbour| !seen.contains(neighbour)) {
                depth_cut = true;
            }
            continue;
        }
        for neighbour in neighbours {
            if !seen.contains(&neighbour) {
                queue.push_back((neighbour, depth + 1));
            }
        }
    }
    let mut node_positions: Vec<usize> = seen
        .iter()
        .filter_map(|id| snapshot.index.callable_by_id.get(id).copied())
        .collect();
    node_positions.sort_unstable();
    let nodes: Vec<CallableSymbol> = node_positions
        .into_iter()
        .map(|position| snapshot.callables[position].clone())
        .collect();
    let edges: Vec<CallSite> = edge_positions
        .into_iter()
        .map(|position| snapshot.calls[position].clone())
        .collect();
    let nodes = page(nodes, None, request.limit, snapshot)?;
    let edges = page(edges, None, request.limit, snapshot)?;
    let mut truncated_by = Vec::new();
    if depth_cut {
        truncated_by.push(TraversalLimit::Depth);
    }
    if nodes.metadata.truncated {
        truncated_by.push(TraversalLimit::NodeLimit);
    }
    if edges.metadata.truncated {
        truncated_by.push(TraversalLimit::EdgeLimit);
    }
    if visit_cut {
        truncated_by.push(TraversalLimit::VisitBudget);
    }
    Ok(TraversalResponse {
        nodes,
        edges,
        truncated_by,
    })
}

pub(crate) fn page<T: Clone>(
    items: Vec<T>,
    cursor: Option<String>,
    limit: u32,
    snapshot: &GraphSnapshot,
) -> Result<Page<T>, CallGraphError> {
    let start = match cursor {
        Some(cursor) => decode_cursor(&cursor, &snapshot.id)?,
        None => 0,
    };
    if start > items.len() {
        return Err(CallGraphError::InvalidRequest(
            "cursor is out of range".into(),
        ));
    }
    let limit = limit.clamp(1, PAGE_LIMIT) as usize;
    let truncated = start + limit < items.len();
    let next_cursor = truncated.then(|| encode_cursor(&snapshot.id, start + limit));
    Ok(Page {
        items: items.into_iter().skip(start).take(limit).collect(),
        next_cursor,
        metadata: QueryMetadata {
            truncated,
            ..metadata(snapshot)
        },
    })
}

fn encode_cursor(snapshot: &SnapshotId, offset: usize) -> String {
    format!("{}:{offset}", snapshot.0)
}

fn decode_cursor(cursor: &str, snapshot: &SnapshotId) -> Result<usize, CallGraphError> {
    let (cursor_snapshot, offset) = cursor
        .rsplit_once(':')
        .ok_or_else(|| CallGraphError::InvalidRequest("invalid cursor".into()))?;
    if cursor_snapshot != snapshot.0 {
        return Err(CallGraphError::InvalidRequest(
            "cursor belongs to a different snapshot".into(),
        ));
    }
    offset
        .parse()
        .map_err(|_| CallGraphError::InvalidRequest("invalid cursor".into()))
}
