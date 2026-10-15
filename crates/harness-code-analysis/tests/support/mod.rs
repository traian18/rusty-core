//! Shared fixture harness: writes a small workspace, refreshes the real call graph, and offers
//! assertions over the public `CallGraph` contract.
#![allow(dead_code)]

pub mod fakes;

use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_code_analysis::{
    CallGraph, CallSite, CallableSymbol, CallsFromRequest, CallsToRequest, DefaultCallGraph,
    FindCallablesRequest, RefreshRequest, RefreshResponse, Resolution, SnapshotId,
};
use tokio_util::sync::CancellationToken;

pub struct Fixture {
    pub root: PathBuf,
    pub graph: DefaultCallGraph,
    pub refresh: RefreshResponse,
    pub snapshot: SnapshotId,
    pub callables: Vec<CallableSymbol>,
    pub sources: BTreeMap<String, String>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("call-graph-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn write_files(root: &std::path::Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
}

impl Fixture {
    pub async fn new(files: &[(&str, &str)]) -> Self {
        let root = temp_root("fixture");
        write_files(&root, files);
        Self::at(root, files).await
    }

    pub async fn at(root: PathBuf, files: &[(&str, &str)]) -> Self {
        let graph = DefaultCallGraph::new(&root);
        let mut fixture = Self {
            root,
            refresh: refresh(&graph).await,
            graph,
            snapshot: SnapshotId(String::new()),
            callables: Vec::new(),
            sources: files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                .collect(),
        };
        fixture.reload().await;
        fixture
    }

    pub async fn reload(&mut self) {
        self.snapshot = self.refresh.snapshot.clone();
        self.callables = all_callables(&self.graph, &self.snapshot).await;
    }

    pub async fn refresh_again(&mut self) {
        self.refresh = refresh(&self.graph).await;
        self.reload().await;
    }

    /// Finds the unique callable with this simple name in `path`.
    pub fn callable(&self, path: &str, name: &str) -> &CallableSymbol {
        let found: Vec<_> = self
            .callables
            .iter()
            .filter(|item| item.span.path == path && item.name == name)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected one callable `{name}` in {path}, found {}: {:?}",
            found.len(),
            self.callables
                .iter()
                .filter(|item| item.span.path == path)
                .map(|item| item.name.clone())
                .collect::<Vec<_>>()
        );
        found[0]
    }

    pub fn callable_by_qualified(&self, path: &str, qualified: &str) -> &CallableSymbol {
        self.callables
            .iter()
            .find(|item| item.span.path == path && item.qualified_name == qualified)
            .unwrap_or_else(|| {
                panic!(
                    "no callable `{qualified}` in {path}: {:?}",
                    self.callables
                        .iter()
                        .filter(|item| item.span.path == path)
                        .map(|item| item.qualified_name.clone())
                        .collect::<Vec<_>>()
                )
            })
    }

    pub async fn calls(&self, caller: &CallableSymbol) -> Vec<CallSite> {
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .graph
                .calls_from(
                    CallsFromRequest {
                        resolutions: Vec::new(),
                        snapshot: self.snapshot.clone(),
                        caller: caller.id.clone(),
                        cursor,
                        limit: 500,
                    },
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .page;
            out.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        out
    }

    pub async fn calls_in(&self, path: &str, caller: &str) -> Vec<CallSite> {
        let caller = self.callable(path, caller).clone();
        self.calls(&caller).await
    }

    pub async fn callers_of(&self, target: &CallableSymbol) -> Vec<CallSite> {
        self.graph
            .calls_to(
                CallsToRequest {
                    resolutions: Vec::new(),
                    snapshot: self.snapshot.clone(),
                    target: target.id.clone(),
                    cursor: None,
                    limit: 500,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .page
            .items
    }

    /// Source text covered by a call site's callee span.
    pub fn callee_text(&self, call: &CallSite) -> &str {
        let text = &self.sources[&call.span.path];
        &text[call.span.start_byte as usize..call.span.end_byte as usize]
    }

    /// All calls from `caller` whose callee text equals `callee`.
    pub async fn sites(&self, path: &str, caller: &str, callee: &str) -> Vec<CallSite> {
        self.calls_in(path, caller)
            .await
            .into_iter()
            .filter(|call| self.callee_text(call) == callee)
            .collect()
    }

    /// The single call from `caller` to `callee`.
    pub async fn site(&self, path: &str, caller: &str, callee: &str) -> CallSite {
        let mut sites = self.sites(path, caller, callee).await;
        assert_eq!(
            sites.len(),
            1,
            "expected one `{callee}` call in {caller}: {:?}",
            self.calls_in(path, caller)
                .await
                .iter()
                .map(|c| self.callee_text(c).to_owned())
                .collect::<Vec<_>>()
        );
        sites.remove(0)
    }

    pub async fn assert_resolved(
        &self,
        path: &str,
        caller: &str,
        callee: &str,
        target_path: &str,
        target_name: &str,
    ) {
        let site = self.site(path, caller, callee).await;
        let target = self.callable(target_path, target_name);
        assert_eq!(
            site.resolution,
            Resolution::Resolved,
            "{callee} in {caller}: {:?}",
            site.evidence
        );
        assert_eq!(
            site.target.as_ref(),
            Some(&target.id),
            "{callee} in {caller}"
        );
        let evidence = site.evidence.expect("resolved calls carry evidence");
        assert!(!evidence.rule_id.is_empty());
        assert!(!evidence.adapter_version.is_empty());
        assert!(!evidence.explanation.is_empty());
    }

    /// Like `assert_resolved` but names the target by qualified name (for duplicate simple names).
    pub async fn assert_resolved_q(
        &self,
        path: &str,
        caller: &str,
        callee: &str,
        target_path: &str,
        target_qualified: &str,
    ) {
        let site = self.site(path, caller, callee).await;
        let target = self.callable_by_qualified(target_path, target_qualified);
        assert_eq!(
            site.resolution,
            Resolution::Resolved,
            "{callee} in {caller}: {:?}",
            site.evidence
        );
        assert_eq!(
            site.target.as_ref(),
            Some(&target.id),
            "{callee} in {caller}"
        );
        let evidence = site.evidence.expect("resolved calls carry evidence");
        assert!(!evidence.rule_id.is_empty() && !evidence.adapter_version.is_empty());
    }

    pub async fn assert_unresolved(&self, path: &str, caller: &str, callee: &str, rule: &str) {
        let site = self.site(path, caller, callee).await;
        assert_eq!(
            site.resolution,
            Resolution::Unresolved,
            "{callee} in {caller} should be unresolved: {:?}",
            site.evidence
        );
        assert!(site.target.is_none());
        assert_eq!(
            site.evidence.as_ref().map(|e| e.rule_id.as_str()),
            Some(rule),
            "{callee} in {caller}: {:?}",
            site.evidence
        );
    }

    pub async fn assert_possible(
        &self,
        path: &str,
        caller: &str,
        callee: &str,
        targets: &[(&str, &str)],
    ) {
        let site = self.site(path, caller, callee).await;
        assert_eq!(
            site.resolution,
            Resolution::Possible,
            "{callee} in {caller}: {:?}",
            site.evidence
        );
        assert!(site.target.is_none());
        let mut expected: Vec<_> = targets
            .iter()
            .map(|(p, n)| self.callable(p, n).id.clone())
            .collect();
        expected.sort();
        let mut actual = site.candidates.clone();
        actual.sort();
        assert_eq!(actual, expected, "{callee} in {caller}");
    }

    /// A possible edge with `count` candidates that all live in `target_path`.
    pub async fn assert_possible_in(
        &self,
        path: &str,
        caller: &str,
        callee: &str,
        count: usize,
        target_path: &str,
    ) {
        let site = self.site(path, caller, callee).await;
        assert_eq!(
            site.resolution,
            Resolution::Possible,
            "{callee} in {caller}: {:?}",
            site.evidence
        );
        assert!(site.target.is_none());
        assert_eq!(site.candidates.len(), count, "{callee} in {caller}");
        for candidate in &site.candidates {
            let callable = self.callables.iter().find(|c| &c.id == candidate).unwrap();
            assert_eq!(callable.span.path, target_path, "{callee} in {caller}");
        }
    }

    pub async fn assert_external(&self, path: &str, caller: &str, callee: &str, module: &str) {
        let site = self.site(path, caller, callee).await;
        assert_eq!(site.resolution, Resolution::Unresolved);
        assert_eq!(
            site.external.as_deref(),
            Some(module),
            "{callee} in {caller}"
        );
        assert!(site.target.is_none());
    }
}

pub async fn refresh(graph: &DefaultCallGraph) -> RefreshResponse {
    graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap()
}

pub async fn all_callables(graph: &DefaultCallGraph, snapshot: &SnapshotId) -> Vec<CallableSymbol> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = graph
            .find_callables(
                FindCallablesRequest {
                    name: None,
                    path: None,
                    snapshot: Some(snapshot.clone()),
                    language: None,
                    cursor,
                    limit: 500,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .page;
        out.extend(page.items);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    out
}
