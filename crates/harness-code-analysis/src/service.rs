use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::adapters::{AdapterRegistry, ExtractedFile};
use crate::api::{CallGraph, CallGraphError};
use crate::builder::CallGraphBuilder;
use crate::model::*;
use crate::snapshot_repository::{GraphSnapshot, SnapshotRepository};
use crate::source::{AnalysisSource, SourceFile};

const PAGE_LIMIT: u32 = 500;
const DEPTH_LIMIT: u32 = 32;

type ExtractionCacheKey = (String, String, String);
type ExtractionCache = BTreeMap<ExtractionCacheKey, Arc<ExtractedFile>>;

pub struct DefaultCallGraph {
    root: PathBuf,
    source: Arc<dyn AnalysisSource>,
    snapshots: Arc<dyn SnapshotRepository>,
    adapters: AdapterRegistry,
    extraction_cache: std::sync::Mutex<ExtractionCache>,
    staged_cache: std::sync::Mutex<ExtractionCache>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl DefaultCallGraph {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::builder(root)
            .build()
            .expect("built-in dependencies are valid")
    }

    pub fn builder(root: impl Into<PathBuf>) -> CallGraphBuilder {
        CallGraphBuilder::new(root)
    }

    pub(crate) fn with_dependencies(
        root: impl Into<PathBuf>,
        source: Arc<dyn AnalysisSource>,
        snapshots: Arc<dyn SnapshotRepository>,
        adapters: AdapterRegistry,
    ) -> Self {
        Self {
            root: root.into(),
            source,
            snapshots,
            adapters,
            extraction_cache: std::sync::Mutex::new(BTreeMap::new()),
            staged_cache: std::sync::Mutex::new(BTreeMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn snapshot(&self, id: &SnapshotId) -> Result<Arc<GraphSnapshot>, CallGraphError> {
        self.snapshots.get(id)
    }
}

#[async_trait]
impl CallGraph for DefaultCallGraph {
    async fn refresh(
        &self,
        request: RefreshRequest,
        cancel: CancellationToken,
    ) -> Result<RefreshResponse, CallGraphError> {
        let _guard = self.refresh_lock.lock().await;
        self.staged_cache
            .lock()
            .map_err(|_| CallGraphError::DependencyFailure("staged cache lock poisoned".into()))?
            .clear();
        let max_files = request.max_files.unwrap_or(10_000).min(10_000) as usize;
        let files = self.source.discover(&self.root, max_files, &cancel).await?;
        let snapshot = self.build_snapshot(files, &cancel)?;
        let staged = self.take_staged_cache()?;
        let response = RefreshResponse {
            snapshot: snapshot.id.clone(),
            metadata: metadata(&snapshot),
        };
        if let Err(error) = self.snapshots.publish(snapshot) {
            self.restore_staged_cache(staged)?;
            return Err(error);
        }
        self.commit_cache(staged)?;
        Ok(response)
    }

    fn capabilities(&self) -> CallGraphCapabilities {
        CallGraphCapabilities {
            languages: self.adapters.capabilities(),
            max_page_size: PAGE_LIMIT,
            max_traversal_depth: DEPTH_LIMIT,
            supports_partial_results: true,
            supports_cross_file_resolution: true,
        }
    }

    async fn find_callables(
        &self,
        request: FindCallablesRequest,
        cancel: CancellationToken,
    ) -> Result<CallableResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        let snapshot_id = request
            .snapshot
            .clone()
            .or_else(|| self.current_id())
            .ok_or_else(|| CallGraphError::SnapshotNotFound(SnapshotId("current".into())))?;
        let snapshot = self.snapshot(&snapshot_id)?;
        let items = snapshot
            .callables
            .iter()
            .filter(|item| {
                request
                    .language
                    .as_ref()
                    .map_or(true, |language| language == &item.language)
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(CallableResponse {
            page: page(items, request.cursor, request.limit, &snapshot)?,
        })
    }

    async fn calls_from(
        &self,
        request: CallsFromRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        let snapshot = self.snapshot(&request.snapshot)?;
        let items = snapshot
            .calls
            .iter()
            .filter(|call| call.caller == request.caller)
            .cloned()
            .collect();
        Ok(CallsResponse {
            page: page(items, request.cursor, request.limit, &snapshot)?,
        })
    }

    async fn calls_to(
        &self,
        request: CallsToRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        let snapshot = self.snapshot(&request.snapshot)?;
        let items = snapshot
            .calls
            .iter()
            .filter(|call| call.target.as_ref() == Some(&request.target))
            .cloned()
            .collect();
        Ok(CallsResponse {
            page: page(items, request.cursor, request.limit, &snapshot)?,
        })
    }

    async fn traverse(
        &self,
        request: TraversalRequest,
        cancel: CancellationToken,
    ) -> Result<TraversalResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        if request.depth > DEPTH_LIMIT {
            return Err(CallGraphError::LimitExceeded("traversal depth".into()));
        }
        let snapshot = self.snapshot(&request.snapshot)?;
        let mut seen = BTreeSet::new();
        let mut queue = request
            .roots
            .into_iter()
            .map(|id| (id, 0))
            .collect::<VecDeque<_>>();
        let mut edges = Vec::new();
        while let Some((id, depth)) = queue.pop_front() {
            if !seen.insert(id.clone()) || depth > request.depth {
                continue;
            }
            for call in snapshot.calls.iter().filter(|call| call.caller == id) {
                edges.push(call.clone());
                if let Some(target) = &call.target {
                    queue.push_back((target.clone(), depth + 1));
                }
            }
        }
        let nodes = snapshot
            .callables
            .iter()
            .filter(|node| seen.contains(&node.id))
            .cloned()
            .collect::<Vec<_>>();
        Ok(TraversalResponse {
            nodes: page(nodes, None, request.limit, &snapshot)?,
            edges: page(edges, None, request.limit, &snapshot)?,
        })
    }
}

impl DefaultCallGraph {
    fn current_id(&self) -> Option<SnapshotId> {
        self.snapshots.current().map(|snapshot| snapshot.id.clone())
    }

    fn build_snapshot(
        &self,
        files: Vec<SourceFile>,
        cancel: &CancellationToken,
    ) -> Result<GraphSnapshot, CallGraphError> {
        let mut extracted = Vec::new();
        let mut diagnostics = Vec::new();
        let mut cache_hits = 0usize;
        for file in &files {
            check_cancelled(cancel)?;
            let Some(adapter) = self.adapters.adapter_for_path(&file.path) else {
                continue;
            };
            let text = match std::str::from_utf8(&file.bytes) {
                Ok(text) => text,
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        path: Some(file.path.clone()),
                        message: format!("invalid UTF-8 source: {error}"),
                    });
                    continue;
                }
            };
            let key = (
                file.path.clone(),
                file.hash.clone(),
                adapter.version().into(),
            );
            let cached = self
                .extraction_cache
                .lock()
                .map_err(|_| {
                    CallGraphError::DependencyFailure("extraction cache lock poisoned".into())
                })?
                .get(&key)
                .cloned();
            let facts = if let Some(facts) = cached {
                cache_hits += 1;
                facts
            } else {
                let facts = Arc::new(adapter.extract(&file.path, text)?);
                self.staged_cache
                    .lock()
                    .map_err(|_| {
                        CallGraphError::DependencyFailure("staged cache lock poisoned".into())
                    })?
                    .insert(key, facts.clone());
                facts
            };
            diagnostics.extend(facts.diagnostics.clone());
            extracted.push((file.path.clone(), facts));
        }
        let (callables, calls) = self.resolve(&extracted);
        check_cancelled(cancel)?;
        let id = SnapshotId(format!(
            "snapshot-{}",
            files
                .iter()
                .map(|file| file.hash.as_str())
                .collect::<Vec<_>>()
                .join("-")
        ));
        let partial = !diagnostics.is_empty() || extracted.len() != files.len();
        tracing::info!(
            files = files.len(),
            callables = callables.len(),
            calls = calls.len(),
            cache_hits,
            partial,
            "call graph refresh completed"
        );
        let snapshot = GraphSnapshot {
            id,
            callables,
            calls,
            partial,
            diagnostics,
            limits: vec![
                "file count, file size, total bytes, page size, candidates, cache, and traversal depth are bounded".into(),
            ],
            freshness: "fresh-after-refresh".into(),
        };
        Ok(snapshot)
    }

    fn take_staged_cache(&self) -> Result<ExtractionCache, CallGraphError> {
        let mut staged = self
            .staged_cache
            .lock()
            .map_err(|_| CallGraphError::DependencyFailure("staged cache lock poisoned".into()))?;
        Ok(std::mem::take(&mut *staged))
    }

    fn restore_staged_cache(&self, staged: ExtractionCache) -> Result<(), CallGraphError> {
        let mut pending = self
            .staged_cache
            .lock()
            .map_err(|_| CallGraphError::DependencyFailure("staged cache lock poisoned".into()))?;
        pending.extend(staged);
        Ok(())
    }

    fn commit_cache(&self, staged: ExtractionCache) -> Result<(), CallGraphError> {
        const CACHE_LIMIT: usize = 20_000;
        let mut cache = self.extraction_cache.lock().map_err(|_| {
            CallGraphError::DependencyFailure("extraction cache lock poisoned".into())
        })?;
        cache.extend(staged);
        while cache.len() > CACHE_LIMIT {
            let Some(key) = cache.keys().next().cloned() else {
                break;
            };
            cache.remove(&key);
        }
        Ok(())
    }

    fn resolve(
        &self,
        files: &[(String, Arc<ExtractedFile>)],
    ) -> (Vec<CallableSymbol>, Vec<CallSite>) {
        let mut callables = Vec::new();
        let mut by_name: BTreeMap<String, Vec<(String, SymbolId)>> = BTreeMap::new();
        let mut owners = BTreeMap::new();
        for (path, file) in files {
            for fact in &file.callables {
                let id = SymbolId(format!("{}:{}:{}", path, fact.start, fact.name));
                let owner = fact
                    .owner
                    .as_ref()
                    .map(|name| SymbolId(format!("{}:owner:{}", path, name)));
                owners.insert((path.clone(), fact.name.clone()), id.clone());
                by_name
                    .entry(fact.name.clone())
                    .or_default()
                    .push((path.clone(), id.clone()));
                callables.push(CallableSymbol {
                    id,
                    name: fact.name.clone(),
                    language: file.language.clone(),
                    span: SourceSpan {
                        path: path.clone(),
                        start_byte: fact.start,
                        end_byte: fact.end,
                    },
                    owner,
                });
            }
        }
        callables.sort_by(|left, right| {
            left.span
                .path
                .cmp(&right.span.path)
                .then(left.span.start_byte.cmp(&right.span.start_byte))
        });
        let mut calls = Vec::new();
        for (path, file) in files {
            for fact in &file.calls {
                let Some(caller_name) = &fact.caller else {
                    continue;
                };
                let Some(caller) = owners.get(&(path.clone(), caller_name.clone())).cloned() else {
                    continue;
                };
                let (target, resolution, rule, explanation) =
                    resolve_target(path, file, fact, &by_name, self.adapters.resolver());
                calls.push(CallSite {
                    caller,
                    span: SourceSpan {
                        path: path.clone(),
                        start_byte: fact.start,
                        end_byte: fact.end,
                    },
                    target,
                    resolution,
                    evidence: Some(Evidence {
                        rule_id: rule.into(),
                        adapter_version: "profile-1".into(),
                        explanation: explanation.into(),
                    }),
                });
            }
        }
        calls.sort_by(|left, right| {
            left.span
                .path
                .cmp(&right.span.path)
                .then(left.span.start_byte.cmp(&right.span.start_byte))
        });
        (callables, calls)
    }
}

fn resolve_target(
    path: &str,
    file: &ExtractedFile,
    call: &crate::adapters::CallFact,
    by_name: &BTreeMap<String, Vec<(String, SymbolId)>>,
    resolver: &dyn crate::adapters::LanguageResolver,
) -> (Option<SymbolId>, Resolution, &'static str, &'static str) {
    let local = by_name
        .get(&call.name)
        .and_then(|items| unique(items.iter().filter(|(candidate, _)| candidate == path)));
    if let Some(target) = local {
        return (
            Some(target),
            Resolution::Resolved,
            "same-file-lexical",
            "resolved to the unique same-file callable",
        );
    }
    if call.qualifier.is_some() {
        return (
            None,
            Resolution::Unresolved,
            "dynamic-receiver",
            "receiver type is not statically known",
        );
    }
    let imported = resolver.imported_name(file, &call.name);
    if imported.is_none() {
        return (
            None,
            Resolution::Unresolved,
            "unimported-cross-file",
            "cross-file symbols require a supported static import",
        );
    }
    let lookup = imported.map_or(call.name.as_str(), |item| item.imported.as_str());
    let candidates = by_name.get(lookup).map(Vec::as_slice).unwrap_or_default();
    let filtered = candidates.iter().filter(|(candidate, _)| {
        imported.map_or(true, |item| module_matches(&item.module, candidate))
    });
    let count = filtered.clone().count();
    match (count, unique(filtered)) {
        (1, Some(target)) => (
            Some(target),
            Resolution::Resolved,
            "static-import",
            "resolved through a unique static import or workspace symbol",
        ),
        (0, _) => (
            None,
            Resolution::Unresolved,
            "unresolved-static",
            "no supported static target was found",
        ),
        _ => (
            None,
            Resolution::Possible,
            "ambiguous-static",
            "multiple static targets remain possible",
        ),
    }
}

fn unique<'a>(mut items: impl Iterator<Item = &'a (String, SymbolId)>) -> Option<SymbolId> {
    let first = items.next()?.1.clone();
    items.next().is_none().then_some(first)
}

fn module_matches(module: &str, path: &str) -> bool {
    let normalized = module.trim_start_matches('.').replace(['.', ':'], "/");
    path.trim_end_matches(
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or(""),
    )
    .trim_end_matches('.')
    .trim_end_matches('/')
    .ends_with(normalized.trim_end_matches(['/', ';']))
}

fn check_cancelled(cancel: &CancellationToken) -> Result<(), CallGraphError> {
    if cancel.is_cancelled() {
        Err(CallGraphError::Cancelled)
    } else {
        Ok(())
    }
}
fn metadata(snapshot: &GraphSnapshot) -> QueryMetadata {
    QueryMetadata {
        snapshot: snapshot.id.clone(),
        partial: snapshot.partial,
        diagnostics: snapshot.diagnostics.clone(),
        truncated: false,
        limits: snapshot.limits.clone(),
        freshness: snapshot.freshness.clone(),
    }
}
fn page<T: Clone>(
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
