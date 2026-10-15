//! Substitute components for the public contracts.
#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_code_analysis::{
    AnalysisSource, CallGraphError, CallableFact, ExtractedFile, GraphSnapshot, LanguageAdapter,
    LanguageCapabilities, LanguageId, SnapshotId, SnapshotRepository, SourceDiscovery, SourceFile,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub fn fake_hash(text: &str) -> String {
    let mut hash = 1469598103934665603u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

/// In-memory source whose next discovery can be held open to model slow or concurrent refreshes.
#[derive(Default)]
pub struct MemorySource {
    pub files: Mutex<Vec<(String, String)>>,
    pub fail: AtomicBool,
    pub block_next: AtomicBool,
    pub entered: Notify,
    pub gate: Notify,
    pub discoveries: AtomicUsize,
}

impl MemorySource {
    pub fn with(files: &[(&str, &str)]) -> Arc<Self> {
        let source = Self::default();
        source.set(files);
        Arc::new(source)
    }

    pub fn set(&self, files: &[(&str, &str)]) {
        *self.files.lock().unwrap() = files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect();
    }
}

#[async_trait]
impl AnalysisSource for MemorySource {
    async fn discover(
        &self,
        _root: &Path,
        _max_files: usize,
        cancel: &CancellationToken,
    ) -> Result<SourceDiscovery, CallGraphError> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        if self.block_next.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.gate.notified().await;
        }
        if cancel.is_cancelled() {
            return Err(CallGraphError::Cancelled);
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(CallGraphError::DependencyFailure(
                "source unavailable".into(),
            ));
        }
        let mut files: Vec<SourceFile> = self
            .files
            .lock()
            .unwrap()
            .iter()
            .map(|(path, text)| SourceFile {
                path: path.clone(),
                bytes: text.clone().into_bytes(),
                hash: fake_hash(text),
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(SourceDiscovery {
            files,
            ..SourceDiscovery::default()
        })
    }
}

/// Rust adapter that emits one callable per `fn name` word and counts extractions. Its version
/// can be changed at runtime to model adapter upgrades.
pub struct CountingAdapter {
    pub version: Mutex<&'static str>,
    pub extractions: AtomicUsize,
    pub fail_on: Mutex<Option<String>>,
    pub fail_with: Mutex<CallGraphError>,
}

impl CountingAdapter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            version: Mutex::new("count-1"),
            extractions: AtomicUsize::new(0),
            fail_on: Mutex::new(None),
            fail_with: Mutex::new(CallGraphError::DependencyFailure("adapter crashed".into())),
        })
    }
}

impl LanguageAdapter for CountingAdapter {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }

    fn version(&self) -> &'static str {
        *self.version.lock().unwrap()
    }

    fn capabilities(&self) -> LanguageCapabilities {
        LanguageCapabilities {
            language: LanguageId::Rust,
            supported: true,
            limitations: vec!["test adapter".into()],
            adapter_version: String::new(),
            resolver_version: String::new(),
        }
    }

    fn extract(&self, path: &str, source: &str) -> Result<ExtractedFile, CallGraphError> {
        self.extractions.fetch_add(1, Ordering::SeqCst);
        if self.fail_on.lock().unwrap().as_deref() == Some(path) {
            return Err(self.fail_with.lock().unwrap().clone());
        }
        let mut file = ExtractedFile::empty(LanguageId::Rust);
        let mut offset = 0usize;
        for word in source.split_whitespace() {
            let start = source[offset..].find(word).unwrap() + offset;
            offset = start + word.len();
            if let Some(name) = word.strip_prefix("call:") {
                let caller = file.callables.last().map(|c| c.key).unwrap_or(0);
                let start = start as u32 + 5;
                file.calls.push(harness_code_analysis::CallFact {
                    caller,
                    name: name.to_owned(),
                    kind: harness_code_analysis::CallKind::Function,
                    receiver: harness_code_analysis::Receiver::None,
                    start,
                    end: start + name.len() as u32,
                });
            }
            if let Some(name) = word.strip_prefix("fn:") {
                let start = start as u32 + 3;
                file.callables.push(CallableFact {
                    key: start,
                    name: name.to_owned(),
                    kind: harness_code_analysis::CallableKind::Function,
                    container: None,
                    namespace: String::new(),
                    parent: None,
                    start,
                    end: start + name.len() as u32,
                    node_start: start,
                    node_end: start + name.len() as u32,
                    locals: Vec::new(),
                });
            }
        }
        Ok(file)
    }
}

/// Repository that can be made to fail publication, wrapping a real one.
pub struct FlakyRepository {
    pub inner: Arc<dyn SnapshotRepository>,
    pub fail_publish: AtomicBool,
    pub published: AtomicUsize,
}

impl FlakyRepository {
    pub fn new(inner: Arc<dyn SnapshotRepository>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            fail_publish: AtomicBool::new(false),
            published: AtomicUsize::new(0),
        })
    }
}

impl SnapshotRepository for FlakyRepository {
    fn current(&self) -> Option<Arc<GraphSnapshot>> {
        self.inner.current()
    }

    fn publish(&self, snapshot: GraphSnapshot) -> Result<(), CallGraphError> {
        if self.fail_publish.load(Ordering::SeqCst) {
            return Err(CallGraphError::DependencyFailure(
                "repository unavailable".into(),
            ));
        }
        self.published.fetch_add(1, Ordering::SeqCst);
        self.inner.publish(snapshot)
    }

    fn get(&self, id: &SnapshotId) -> Result<Arc<GraphSnapshot>, CallGraphError> {
        self.inner.get(id)
    }
}
