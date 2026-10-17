//! Extraction, caching, and resolution orchestration for one refresh.
//!
//! A refresh runs four steps: extract facts from every source file (cached by
//! content), load compiler configs, resolve facts with each language's resolver,
//! and assemble the immutable [`ImportIndex`].
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    index::{ComponentVersion, ImportIndex, ImportIndexParts, IndexedConfig, IndexedFile},
    metrics::{ImportMetrics, ImportPhaseDurations},
    model::{
        ConfigFile, ExtractedFile, ImportBuildError, ImportEdge, ImportFact, ImportFactId,
        ImportLimits, ImportResolver, LanguageImportAdapter, LanguageProfile, ResolveContext,
        ResolveOutput, SourceUnit,
    },
};
use crate::repository_intelligence::{
    read_contained_file, DiscoveryDiagnostic, DiscoveryError, PackageId, RepositoryStructure,
    RootRelativePath,
};

/// One language's extraction and resolution, registered together.
#[derive(Clone)]
pub struct ImportSupport {
    pub adapter: Arc<dyn LanguageImportAdapter>,
    pub resolver: Arc<dyn ImportResolver>,
}

type ContentHash = [u8; 32];
type CacheKey = (String, ContentHash, &'static str, u32, usize);
type CacheEntries = (HashMap<CacheKey, Arc<ExtractedFile>>, VecDeque<CacheKey>);

/// Bounded per-file extraction cache; entries are pure functions of their key.
pub struct ExtractionCache {
    capacity: usize,
    entries: Mutex<CacheEntries>,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// Cumulative cache counters since the cache was created.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

impl ExtractionCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    fn get(&self, key: &CacheKey) -> Option<Arc<ExtractedFile>> {
        let found = self
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.0.get(key).cloned());
        let counter = if found.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    fn insert(&self, key: CacheKey, value: Arc<ExtractedFile>) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.0.insert(key.clone(), value).is_none() {
            entries.1.push_back(key);
        }
        while entries.1.len() > self.capacity {
            if let Some(oldest) = entries.1.pop_front() {
                entries.0.remove(&oldest);
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Ecosystem {
    Cargo,
    Npm,
    Python,
    Go,
    Java,
    DotNet,
    Composer,
    Native,
}

fn ecosystem(profile: LanguageProfile) -> Ecosystem {
    match profile {
        LanguageProfile::Rust => Ecosystem::Cargo,
        LanguageProfile::TypeScript | LanguageProfile::JavaScript => Ecosystem::Npm,
        LanguageProfile::Python => Ecosystem::Python,
        LanguageProfile::Go => Ecosystem::Go,
        LanguageProfile::Java => Ecosystem::Java,
        LanguageProfile::CSharp => Ecosystem::DotNet,
        LanguageProfile::Php => Ecosystem::Composer,
        LanguageProfile::C | LanguageProfile::Cpp => Ecosystem::Native,
    }
}

/// Problems collected while building one index.
#[derive(Default)]
struct BuildReport {
    diagnostics: Vec<DiscoveryDiagnostic>,
    unattached: Vec<RootRelativePath>,
    partial: bool,
}

impl BuildReport {
    /// Records a diagnostic and marks coverage as incomplete.
    fn omit(&mut self, path: &RootRelativePath, message: impl Into<String>) {
        self.diagnostics
            .push(DiscoveryDiagnostic::for_path(path, message));
        self.partial = true;
    }
}

/// Extracted sources and the index rows describing them.
struct Extraction {
    units: Vec<SourceUnit>,
    files: Vec<IndexedFile>,
    bytes: u64,
}

/// A finished index and what it took to build.
pub struct ImportBuild {
    pub index: ImportIndex,
    pub metrics: ImportMetrics,
}

/// Builds import indexes for a set of registered languages.
pub struct ImportPipeline {
    supports: Vec<ImportSupport>,
    limits: ImportLimits,
    cache: ExtractionCache,
}

impl ImportPipeline {
    pub fn new(supports: Vec<ImportSupport>, limits: ImportLimits) -> Self {
        Self {
            supports,
            limits,
            cache: ExtractionCache::new(100_000),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.supports.is_empty()
    }

    pub fn supports(&self) -> &[ImportSupport] {
        &self.supports
    }

    pub fn limits(&self) -> &ImportLimits {
        &self.limits
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    pub fn build(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        cancel: &CancellationToken,
    ) -> Result<ImportBuild, DiscoveryError> {
        let mut report = BuildReport::default();
        let cache_before = self.cache.stats();
        let started = Instant::now();
        let extraction = self.extract_sources(root, structure, cancel, &mut report)?;
        let extracted = started.elapsed();
        let (configs, indexed_configs) = self.load_configs(root, structure, cancel, &mut report)?;
        let configured = started.elapsed();
        self.flag_languages_without_resolver(&extraction.units, &mut report);
        let edges = self.resolve_all(
            root,
            structure,
            &extraction.units,
            &configs,
            cancel,
            &mut report,
        )?;
        let resolved = started.elapsed();
        if cancel.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }
        let index = ImportIndex::new(ImportIndexParts {
            files: extraction.files,
            configs: indexed_configs,
            unattached: report.unattached,
            edges,
            diagnostics: report.diagnostics,
            partial: report.partial,
            limits: self.limits.clone(),
            components: self.component_versions(),
        });
        let durations = ImportPhaseDurations {
            extraction: extracted,
            config_loading: configured - extracted,
            resolution: resolved - configured,
            assembly: started.elapsed() - resolved,
        };
        let cache = self.cache.stats();
        let cache_delta = CacheStats {
            hits: cache.hits - cache_before.hits,
            misses: cache.misses - cache_before.misses,
        };
        let metrics = ImportMetrics::from_index(&index, extraction.bytes, cache_delta, durations);
        Ok(ImportBuild { index, metrics })
    }

    /// Reads every inventoried source an adapter handles and extracts its facts.
    fn extract_sources(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        cancel: &CancellationToken,
        report: &mut BuildReport,
    ) -> Result<Extraction, DiscoveryError> {
        let package_dirs = package_directories(structure);
        let mut extraction = Extraction {
            units: Vec::new(),
            files: Vec::new(),
            bytes: 0,
        };
        let mut total_facts = 0usize;
        for path in &structure.source_files {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let Some((support, profile)) = self.support_for(path) else {
                continue;
            };
            let Some((text, hash)) = self.read_text(root, path, "source", report) else {
                continue;
            };
            extraction.bytes += text.len() as u64;
            let extracted = self.extract_cached(support, path, profile, &text, hash);
            if total_facts.saturating_add(extracted.facts.len()) > self.limits.max_total_facts {
                report.omit(
                    path,
                    "total import fact limit reached; remaining files were not indexed",
                );
                break;
            }
            total_facts += extracted.facts.len();
            note_extraction_issues(path, &extracted, report);
            let package = owning_package(&package_dirs, ecosystem(profile), path);
            extraction.files.push(IndexedFile {
                path: path.clone(),
                package: package.clone(),
                profile,
                content_hash: hex(&hash),
                fact_count: extracted.facts.len(),
            });
            extraction.units.push(SourceUnit {
                path: path.clone(),
                package,
                profile,
                extracted,
            });
        }
        Ok(extraction)
    }

    fn support_for(&self, path: &RootRelativePath) -> Option<(&ImportSupport, LanguageProfile)> {
        self.supports
            .iter()
            .find_map(|support| support.adapter.profile_for(path).map(|p| (support, p)))
    }

    /// Reads a UTF-8 file within the byte limit, recording why it was skipped otherwise.
    fn read_text(
        &self,
        root: &Path,
        path: &RootRelativePath,
        what: &str,
        report: &mut BuildReport,
    ) -> Option<(String, ContentHash)> {
        let bytes = match read_contained_file(root, path.as_path(), self.limits.max_file_bytes) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
                report.omit(path, format!("{what} exceeds configured import byte limit"));
                return None;
            }
            Err(error) => {
                report.omit(path, error.to_string());
                return None;
            }
        };
        let hash: ContentHash = Sha256::digest(&bytes).into();
        match String::from_utf8(bytes) {
            Ok(text) => Some((text, hash)),
            Err(_) => {
                report.omit(path, format!("{what} is not valid UTF-8"));
                None
            }
        }
    }

    fn extract_cached(
        &self,
        support: &ImportSupport,
        path: &RootRelativePath,
        profile: LanguageProfile,
        text: &str,
        hash: ContentHash,
    ) -> Arc<ExtractedFile> {
        let key = (
            path.as_path().to_string_lossy().into_owned(),
            hash,
            support.adapter.id(),
            support.adapter.version(),
            self.limits.max_facts_per_file,
        );
        if let Some(cached) = self.cache.get(&key) {
            return cached;
        }
        let fresh = Arc::new(support.adapter.extract(path, profile, text, &self.limits));
        self.cache.insert(key, fresh.clone());
        fresh
    }

    /// Loads `tsconfig`-style files for resolvers; unreadable ones leave a diagnostic.
    fn load_configs(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        cancel: &CancellationToken,
        report: &mut BuildReport,
    ) -> Result<(Vec<ConfigFile>, Vec<IndexedConfig>), DiscoveryError> {
        let (mut configs, mut indexed) = (Vec::new(), Vec::new());
        for path in &structure.config_files {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let Some((text, hash)) = self.read_text(root, path, "config", report) else {
                continue;
            };
            indexed.push(IndexedConfig {
                path: path.clone(),
                content_hash: hex(&hash),
            });
            configs.push(ConfigFile {
                path: path.clone(),
                text,
            });
        }
        Ok((configs, indexed))
    }

    fn flag_languages_without_resolver(&self, units: &[SourceUnit], report: &mut BuildReport) {
        let handled: HashSet<LanguageProfile> = self
            .supports
            .iter()
            .flat_map(|support| support.resolver.profiles())
            .collect();
        for unit in units {
            if !handled.contains(&unit.profile) && !unit.extracted.facts.is_empty() {
                report.omit(&unit.path, "no resolver registered for profile");
            }
        }
    }

    /// Runs each resolver and turns its answers into edges with package attribution.
    fn resolve_all(
        &self,
        root: &Path,
        structure: &RepositoryStructure,
        units: &[SourceUnit],
        configs: &[ConfigFile],
        cancel: &CancellationToken,
        report: &mut BuildReport,
    ) -> Result<Vec<ImportEdge>, DiscoveryError> {
        let context = ResolveContext {
            root,
            structure,
            configs,
            units,
            limits: &self.limits,
            cancel,
        };
        let packages: HashMap<&RootRelativePath, &Option<PackageId>> =
            units.iter().map(|u| (&u.path, &u.package)).collect();
        let mut edges = Vec::new();
        for support in &self.supports {
            let output = support
                .resolver
                .resolve(&context)
                .map_err(|error| match error {
                    ImportBuildError::Cancelled => DiscoveryError::Cancelled,
                    ImportBuildError::Failed(message) => DiscoveryError::Failed(message),
                })?;
            let facts = facts_for(units, &support.resolver.profiles());
            edges.extend(attribute_edges(
                support.resolver.id(),
                output,
                &facts,
                &packages,
                report,
            )?);
        }
        Ok(edges)
    }

    fn component_versions(&self) -> Vec<ComponentVersion> {
        self.supports
            .iter()
            .flat_map(|support| {
                [
                    ComponentVersion {
                        id: format!("adapter:{}", support.adapter.id()),
                        version: support.adapter.version(),
                    },
                    ComponentVersion {
                        id: format!("resolver:{}", support.resolver.id()),
                        version: support.resolver.version(),
                    },
                ]
            })
            .collect()
    }
}

fn note_extraction_issues(
    path: &RootRelativePath,
    extracted: &ExtractedFile,
    report: &mut BuildReport,
) {
    if extracted.truncated {
        report.omit(path, "file exceeded the per-file import fact limit");
    }
    report.diagnostics.extend(
        extracted
            .diagnostics
            .iter()
            .map(|message| DiscoveryDiagnostic::for_path(path, message.clone())),
    );
}

fn facts_for<'a>(
    units: &'a [SourceUnit],
    profiles: &[LanguageProfile],
) -> HashMap<&'a ImportFactId, &'a ImportFact> {
    units
        .iter()
        .filter(|unit| profiles.contains(&unit.profile))
        .flat_map(|unit| unit.extracted.facts.iter())
        .map(|fact| (&fact.id, fact))
        .collect()
}

/// Pairs every resolved fact with its package attribution; each fact must be answered exactly once.
fn attribute_edges(
    resolver: &str,
    output: ResolveOutput,
    facts: &HashMap<&ImportFactId, &ImportFact>,
    packages: &HashMap<&RootRelativePath, &Option<PackageId>>,
    report: &mut BuildReport,
) -> Result<Vec<ImportEdge>, DiscoveryError> {
    let failure = |what: &str| DiscoveryError::Failed(format!("resolver {resolver} {what}"));
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for resolved in output.resolved {
        let fact = facts
            .get(&resolved.fact)
            .ok_or_else(|| failure("returned an unknown fact"))?;
        if !seen.insert(resolved.fact.clone()) {
            return Err(failure("returned a fact twice"));
        }
        let package_of = |file: &RootRelativePath| packages.get(file).and_then(|p| (*p).clone());
        edges.push(ImportEdge {
            from_package: package_of(&fact.importing_file),
            to_package: resolved.resolution.target_file().and_then(package_of),
            fact: (*fact).clone(),
            resolution: resolved.resolution,
            rule: resolved.rule,
            evidence: resolved.evidence,
        });
    }
    if seen.len() != facts.len() {
        return Err(failure("did not resolve every fact"));
    }
    report.diagnostics.extend(output.diagnostics);
    report.unattached.extend(output.unattached);
    Ok(edges)
}

fn hex(hash: &ContentHash) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn package_directories(
    structure: &RepositoryStructure,
) -> HashMap<(Ecosystem, std::path::PathBuf), PackageId> {
    structure
        .packages
        .iter()
        .filter_map(|package| {
            let ecosystem = manifest_ecosystem(package.manifest.as_path().file_name()?.to_str()?)?;
            let directory = package.manifest.directory().to_path_buf();
            Some(((ecosystem, directory), package.id.clone()))
        })
        .collect()
}

/// The ecosystem a manifest file belongs to, by its file name; `None` for files that name no package.
fn manifest_ecosystem(name: &str) -> Option<Ecosystem> {
    Some(match name {
        "Cargo.toml" => Ecosystem::Cargo,
        "package.json" => Ecosystem::Npm,
        "pyproject.toml" | "setup.py" | "setup.cfg" | "Pipfile" => Ecosystem::Python,
        _ if name.starts_with("requirements") && name.ends_with(".txt") => Ecosystem::Python,
        "go.mod" => Ecosystem::Go,
        "pom.xml" | "build.gradle" | "build.gradle.kts" => Ecosystem::Java,
        _ if name.ends_with(".csproj") => Ecosystem::DotNet,
        "composer.json" => Ecosystem::Composer,
        "CMakeLists.txt" | "Makefile" | "compile_commands.json" => Ecosystem::Native,
        _ => return None,
    })
}

/// The package whose manifest is the nearest ancestor of `path` in the matching ecosystem.
fn owning_package(
    directories: &HashMap<(Ecosystem, std::path::PathBuf), PackageId>,
    ecosystem: Ecosystem,
    path: &RootRelativePath,
) -> Option<PackageId> {
    let mut directory = path.as_path().parent();
    while let Some(current) = directory {
        if let Some(id) = directories.get(&(ecosystem, current.to_path_buf())) {
            return Some(id.clone());
        }
        directory = current.parent();
    }
    None
}
