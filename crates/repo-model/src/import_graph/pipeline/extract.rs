//! Extracting import facts from source files, reading each file once and reusing cached results.

use std::{path::Path, sync::Arc};

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::cache::{hex, ContentHash};
use super::ecosystem::{ecosystem, owning_package, package_directories};
use super::report::{note_extraction_issues, BuildReport, Extraction};
use super::{ImportPipeline, ImportSupport};
use crate::import_graph::{
    index::{IndexedConfig, IndexedFile},
    model::{ConfigFile, ExtractedFile, LanguageProfile, SourceUnit},
};
use crate::repository_intelligence::{
    read_contained_file, DiscoveryError, RepositoryStructure, RootRelativePath,
};

impl ImportPipeline {
    /// Reads every inventoried source an adapter handles and extracts its facts.
    pub(super) fn extract_sources(
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

    pub(super) fn support_for(
        &self,
        path: &RootRelativePath,
    ) -> Option<(&ImportSupport, LanguageProfile)> {
        self.supports
            .iter()
            .find_map(|support| support.adapter.profile_for(path).map(|p| (support, p)))
    }

    /// Reads a UTF-8 file within the byte limit, recording why it was skipped otherwise.
    pub(super) fn read_text(
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

    pub(super) fn extract_cached(
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
    pub(super) fn load_configs(
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
}
