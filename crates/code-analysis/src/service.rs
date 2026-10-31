use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::adapters::{
    AdapterRegistry, ExtractedFile, Outcome, ProjectIndex, ResolutionContext, TargetRef,
};
use crate::api::{CallGraph, CallGraphError};
use crate::builder::CallGraphBuilder;
use crate::model::*;
use crate::query::{self, metadata, DEPTH_LIMIT, PAGE_LIMIT};
use crate::snapshot_repository::{GraphSnapshot, SnapshotParts, SnapshotRepository};
use crate::source::{AnalysisSource, SourceDiscovery, SourceFile};

/// Upper bound on source files analysed by one refresh.
const MAX_FILES: u32 = 10_000;
/// Upper bound on cached per-file extraction results.
const CACHE_LIMIT: usize = 20_000;
/// Maximum candidate targets kept on one ambiguous call site.
const CANDIDATE_LIMIT: usize = 8;

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
        let started = std::time::Instant::now();
        let max_files = match request.max_files {
            Some(0) => {
                return Err(CallGraphError::InvalidRequest(
                    "max_files must be at least 1".into(),
                ))
            }
            Some(requested) => requested.min(MAX_FILES),
            None => MAX_FILES,
        } as usize;
        // Refreshes are serialized; waiting for the lock is cancellable.
        let _guard = cancel
            .run_until_cancelled(self.refresh_lock.lock())
            .await
            .ok_or(CallGraphError::Cancelled)?;
        check_cancelled(&cancel)?;
        let discovery = self.source.discover(&self.root, max_files, &cancel).await?;
        let (snapshot, live_keys) = self.build_snapshot(discovery, &cancel, started).await?;
        let response = RefreshResponse {
            snapshot: snapshot.id.clone(),
            metadata: metadata(&snapshot),
            stats: snapshot.stats.clone(),
        };
        let staged = self.take_staged_cache()?;
        if let Err(error) = self.snapshots.publish(snapshot) {
            self.restore_staged_cache(staged)?;
            return Err(error);
        }
        self.commit_cache(staged, &live_keys)?;
        tracing::info!(
            refresh_duration_ms = started.elapsed().as_millis() as u64,
            files = response.stats.files_discovered,
            analyzed = response.stats.files_analyzed,
            callables = response.stats.callables,
            calls = response.stats.calls,
            unresolved_calls = response.stats.unresolved,
            cache_hits = response.stats.cache_hits,
            cache_misses = response.stats.cache_misses,
            limits_triggered = response.stats.limits_triggered,
            partial = response.metadata.partial,
            diagnostic_count = response.metadata.diagnostics.len(),
            "call graph refresh published"
        );
        for (reason, count) in &response.stats.unresolved_by_reason {
            tracing::debug!(reason = reason.as_str(), count, "unresolved call sites");
        }
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
        query::validate_limit(request.limit)?;
        let snapshot_id = request
            .snapshot
            .clone()
            .or_else(|| self.current_id())
            .ok_or_else(|| CallGraphError::SnapshotNotFound(SnapshotId("current".into())))?;
        let snapshot = self.snapshot(&snapshot_id)?;
        query::find_callables(&snapshot, &request)
    }

    async fn calls_from(
        &self,
        request: CallsFromRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        query::validate_limit(request.limit)?;
        let snapshot = self.snapshot(&request.snapshot)?;
        query::calls_from(&snapshot, &request)
    }

    async fn calls_to(
        &self,
        request: CallsToRequest,
        cancel: CancellationToken,
    ) -> Result<CallsResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        query::validate_limit(request.limit)?;
        let snapshot = self.snapshot(&request.snapshot)?;
        query::calls_to(&snapshot, &request)
    }

    async fn traverse(
        &self,
        request: TraversalRequest,
        cancel: CancellationToken,
    ) -> Result<TraversalResponse, CallGraphError> {
        check_cancelled(&cancel)?;
        query::validate_limit(request.limit)?;
        let snapshot = self.snapshot(&request.snapshot)?;
        query::traverse(&snapshot, &request)
    }
}

impl DefaultCallGraph {
    fn current_id(&self) -> Option<SnapshotId> {
        self.snapshots.current().map(|snapshot| snapshot.id.clone())
    }

    async fn build_snapshot(
        &self,
        discovery: SourceDiscovery,
        cancel: &CancellationToken,
        started: std::time::Instant,
    ) -> Result<(GraphSnapshot, BTreeSet<ExtractionCacheKey>), CallGraphError> {
        let SourceDiscovery {
            files,
            diagnostics: discovery_diagnostics,
            limits: discovery_limits,
        } = discovery;
        let mut stats = RefreshStats {
            files_discovered: files.len() as u32,
            ..RefreshStats::default()
        };
        let mut extracted = Vec::new();
        let mut diagnostics = discovery_diagnostics;
        let mut live_keys = BTreeSet::new();
        for file in &files {
            check_cancelled(cancel)?;
            // Parsing is synchronous; yield so other tasks and cancellation stay responsive.
            tokio::task::yield_now().await;
            stats.source_bytes += file.bytes.len() as u64;
            let Some(adapter) = self.adapters.adapter_for_path(&file.path) else {
                stats.files_skipped += 1;
                continue;
            };
            let text = match std::str::from_utf8(&file.bytes) {
                Ok(text) => text,
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        path: Some(file.path.clone()),
                        message: format!("invalid UTF-8 source: {error}"),
                    });
                    stats.files_skipped += 1;
                    continue;
                }
            };
            let key = (
                file.path.clone(),
                file.hash.clone(),
                adapter.version().to_owned(),
            );
            // Facts staged by an earlier attempt that failed before publishing are reusable too.
            let cached = {
                let committed = self.extraction_cache.lock().map_err(|_| {
                    CallGraphError::DependencyFailure("extraction cache lock poisoned".into())
                })?;
                match committed.get(&key) {
                    Some(facts) => Some(facts.clone()),
                    None => self
                        .staged_cache
                        .lock()
                        .map_err(|_| {
                            CallGraphError::DependencyFailure("staged cache lock poisoned".into())
                        })?
                        .get(&key)
                        .cloned(),
                }
            };
            let facts = if let Some(facts) = cached {
                stats.cache_hits += 1;
                facts
            } else {
                stats.cache_misses += 1;
                match adapter.extract(&file.path, text) {
                    Ok(facts) => {
                        let facts = Arc::new(facts);
                        self.staged_cache
                            .lock()
                            .map_err(|_| {
                                CallGraphError::DependencyFailure(
                                    "staged cache lock poisoned".into(),
                                )
                            })?
                            .insert(key.clone(), facts.clone());
                        facts
                    }
                    Err(CallGraphError::LimitExceeded(message))
                    | Err(CallGraphError::InvalidRequest(message)) => {
                        diagnostics.push(Diagnostic {
                            path: Some(file.path.clone()),
                            message: format!("analysis limit rejected the file: {message}"),
                        });
                        stats.files_skipped += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            live_keys.insert(key);
            stats.files_analyzed += 1;
            diagnostics.extend(facts.diagnostics.clone());
            extracted.push((file.path.clone(), facts));
        }
        check_cancelled(cancel)?;
        tokio::task::yield_now().await;
        let (callables, calls) = self.resolve(&extracted);
        check_cancelled(cancel)?;
        stats.callables = callables.len() as u32;
        stats.calls = calls.len() as u32;
        for call in &calls {
            match call.resolution {
                Resolution::Resolved => stats.resolved += 1,
                Resolution::Possible => stats.possible += 1,
                Resolution::Unresolved => {
                    stats.unresolved += 1;
                    let reason = call
                        .evidence
                        .as_ref()
                        .map_or("unknown", |evidence| evidence.rule_id.as_str());
                    *stats
                        .unresolved_by_reason
                        .entry(reason.to_owned())
                        .or_default() += 1;
                }
            }
        }
        stats.limits_triggered = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("limit"))
            .count() as u32;
        diagnostics.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
        let partial = !diagnostics.is_empty();
        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        let id = self.snapshot_identity(&files);
        let mut limits = discovery_limits;
        limits.push(format!(
            "page_size<={PAGE_LIMIT}; candidates<={CANDIDATE_LIMIT}; traversal_depth<={DEPTH_LIMIT}; \
             traversal_visits<={}; cache<={CACHE_LIMIT}",
            query::VISIT_LIMIT
        ));
        let generated_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let snapshot = GraphSnapshot::new(SnapshotParts {
            id,
            callables,
            calls,
            partial,
            diagnostics,
            limits,
            freshness: "analysed bytes are identified by hash; edits after the refresh are not \
                        reflected until the next refresh"
                .into(),
            generated_at_unix,
            stats,
        });
        Ok((snapshot, live_keys))
    }

    /// Deterministic identity of the analysed inputs and the analysis rules.
    fn snapshot_identity(&self, files: &[SourceFile]) -> SnapshotId {
        let mut hasher = Sha256::new();
        hasher.update(b"call-graph-snapshot-v2\0");
        for capability in self.adapters.capabilities() {
            hasher.update(format!("{:?}", capability.language).as_bytes());
            hasher.update(b"\0");
            hasher.update(capability.adapter_version.as_bytes());
            hasher.update(b"\0");
            hasher.update(capability.resolver_version.as_bytes());
            hasher.update(b"\0");
        }
        for file in files {
            hasher.update(file.path.as_bytes());
            hasher.update(b"\0");
            hasher.update(file.hash.as_bytes());
            hasher.update(b"\0");
        }
        let digest = hasher.finalize();
        let hex: String = digest
            .iter()
            .take(12)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        SnapshotId(format!("snap-{hex}"))
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
        while pending.len() > CACHE_LIMIT {
            let Some(key) = pending.keys().next().cloned() else {
                break;
            };
            pending.remove(&key);
        }
        Ok(())
    }

    /// Merges freshly extracted facts and drops entries no file of the workspace still uses.
    fn commit_cache(
        &self,
        staged: ExtractionCache,
        live: &BTreeSet<ExtractionCacheKey>,
    ) -> Result<(), CallGraphError> {
        let mut cache = self.extraction_cache.lock().map_err(|_| {
            CallGraphError::DependencyFailure("extraction cache lock poisoned".into())
        })?;
        cache.extend(staged);
        cache.retain(|key, _| live.contains(key));
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
        let mut ids: BTreeMap<(String, u32), SymbolId> = BTreeMap::new();
        for (path, file) in files {
            for fact in &file.callables {
                ids.insert(
                    (path.clone(), fact.key),
                    SymbolId(format!("{}:{}:{}", path, fact.key, fact.name)),
                );
            }
        }
        for (path, file) in files {
            for fact in &file.callables {
                let id = ids[&(path.clone(), fact.key)].clone();
                let owner = match fact.parent {
                    Some(parent) => ids.get(&(path.clone(), parent)).cloned(),
                    None => fact
                        .container
                        .as_ref()
                        .map(|name| SymbolId(format!("{}:owner:{}", path, name))),
                };
                callables.push(CallableSymbol {
                    id,
                    name: fact.name.clone(),
                    qualified_name: qualified_name(file, fact),
                    kind: fact.kind,
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
                .then(left.id.cmp(&right.id))
        });
        let project = ProjectIndex::new(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.as_ref())),
        );
        let mut calls = Vec::new();
        for (path, file) in files {
            let adapter_version = self
                .adapters
                .adapter_for(&file.language)
                .map(|adapter| adapter.version())
                .unwrap_or("unknown");
            let resolver = self.adapters.resolver_for(&file.language);
            for fact in &file.calls {
                let Some(caller) = ids.get(&(path.clone(), fact.caller)).cloned() else {
                    continue;
                };
                let ctx = ResolutionContext {
                    path,
                    file,
                    project: &project,
                    namespace: file
                        .callable(fact.caller)
                        .map_or("", |callable| callable.namespace.as_str()),
                };
                let outcome = match resolver {
                    Some(resolver) => resolver.resolve(&ctx, fact),
                    None => Outcome::Unresolved {
                        rule: "no-resolver",
                        explanation: "no resolver is registered for this language".into(),
                    },
                };
                let version = match resolver {
                    Some(resolver) => format!("{adapter_version}+{}", resolver.version()),
                    None => adapter_version.to_owned(),
                };
                calls.push(call_site(
                    caller,
                    SourceSpan {
                        path: path.clone(),
                        start_byte: fact.start,
                        end_byte: fact.end,
                    },
                    outcome,
                    &version,
                    &ids,
                    files,
                ));
            }
        }
        calls.sort_by(|left, right| {
            left.span
                .path
                .cmp(&right.span.path)
                .then(left.span.start_byte.cmp(&right.span.start_byte))
                .then(left.caller.cmp(&right.caller))
        });
        (callables, calls)
    }
}

fn qualified_name(file: &ExtractedFile, fact: &crate::adapters::CallableFact) -> String {
    let mut parts = Vec::new();
    if !fact.namespace.is_empty() {
        parts.push(fact.namespace.clone());
    }
    if let Some(container) = &fact.container {
        parts.push(container.clone());
    }
    let mut chain = vec![fact.name.clone()];
    let mut parent = fact.parent;
    while let Some(key) = parent {
        let Some(callable) = file.callable(key) else {
            break;
        };
        chain.push(callable.name.clone());
        parent = callable.parent;
    }
    chain.reverse();
    parts.extend(chain);
    parts.join(".")
}

fn call_site(
    caller: SymbolId,
    span: SourceSpan,
    outcome: Outcome,
    version: &str,
    ids: &BTreeMap<(String, u32), SymbolId>,
    files: &[(String, Arc<ExtractedFile>)],
) -> CallSite {
    let definition_span = |target: &TargetRef| {
        files
            .iter()
            .find(|(path, _)| *path == target.path)
            .and_then(|(_, file)| file.callable(target.key))
            .map(|callable| SourceSpan {
                path: target.path.clone(),
                start_byte: callable.start,
                end_byte: callable.end,
            })
    };
    let evidence = |rule: &str, explanation: String, span: Option<SourceSpan>| Evidence {
        rule_id: rule.into(),
        adapter_version: version.into(),
        explanation,
        span,
    };
    let site = |target, resolution, evidence, candidates, truncated, external| CallSite {
        caller: caller.clone(),
        span: span.clone(),
        target,
        resolution,
        evidence: Some(evidence),
        candidates,
        candidates_truncated: truncated,
        external,
    };
    match outcome {
        Outcome::Resolved {
            target,
            rule,
            explanation,
            span: evidence_span,
        } => match ids.get(&(target.path.clone(), target.key)) {
            Some(id) => site(
                Some(id.clone()),
                Resolution::Resolved,
                evidence(
                    rule,
                    explanation,
                    evidence_span.or_else(|| definition_span(&target)),
                ),
                Vec::new(),
                false,
                None,
            ),
            None => site(
                None,
                Resolution::Unresolved,
                evidence(
                    "missing-target",
                    "the resolver named a callable that is not in this snapshot".into(),
                    Some(span.clone()),
                ),
                Vec::new(),
                false,
                None,
            ),
        },
        Outcome::Possible {
            targets,
            rule,
            explanation,
            span: evidence_span,
        } => {
            let all: Vec<SymbolId> = targets
                .iter()
                .filter_map(|target| ids.get(&(target.path.clone(), target.key)).cloned())
                .collect();
            let truncated = all.len() > CANDIDATE_LIMIT;
            let candidates = all.into_iter().take(CANDIDATE_LIMIT).collect();
            let explanation = if truncated {
                format!("{explanation} (candidate list truncated at {CANDIDATE_LIMIT})")
            } else {
                explanation
            };
            site(
                None,
                Resolution::Possible,
                evidence(rule, explanation, evidence_span.or(Some(span.clone()))),
                candidates,
                truncated,
                None,
            )
        }
        Outcome::Unresolved { rule, explanation } => site(
            None,
            Resolution::Unresolved,
            evidence(rule, explanation, Some(span.clone())),
            Vec::new(),
            false,
            None,
        ),
        Outcome::External {
            name,
            rule,
            explanation,
            span: evidence_span,
        } => site(
            None,
            Resolution::Unresolved,
            evidence(rule, explanation, evidence_span.or(Some(span.clone()))),
            Vec::new(),
            false,
            Some(name),
        ),
    }
}

fn check_cancelled(cancel: &CancellationToken) -> Result<(), CallGraphError> {
    if cancel.is_cancelled() {
        Err(CallGraphError::Cancelled)
    } else {
        Ok(())
    }
}
