//! Typed import facts, resolutions, and extension contracts.
//!
//! Facts describe what a source file says; edges describe what the resolver could
//! establish from it. The two are kept separate so resolution rules can change
//! without re-extracting.
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::repository_intelligence::{
    DiscoveryDiagnostic, PackageId, RepositoryStructure, RootRelativePath,
};

/// Half-open byte range into the file contents the snapshot was built from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageProfile {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    Kotlin,
    #[serde(rename = "csharp")]
    CSharp,
    Php,
    C,
    Cpp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportForm {
    Use,
    Mod,
    ExternCrate,
    Import,
    ExportFrom,
    ExportAllFrom,
    Require,
    DynamicImport,
    /// `#include` in C and C++.
    Include,
}

/// Whether the import is needed when the code runs. Rust imports are always runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Runtime,
    TypeOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Staticness {
    Static,
    Dynamic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Crate,
    Restricted,
    Private,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Const,
    Static,
    Module,
    Macro,
    Class,
    Interface,
    Variable,
}

/// How a Rust `mod` declaration names its file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathAttribute {
    #[default]
    None,
    Literal(String),
    /// A `path` attribute whose value is not a plain string literal.
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ImportFactId(pub String);

/// One import site, independent of any resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportFact {
    pub id: ImportFactId,
    pub importing_file: RootRelativePath,
    /// The specifier or path as written (full path for a Rust `use` leaf).
    pub spelling: String,
    pub form: ImportForm,
    pub span: ByteSpan,
    pub profile: LanguageProfile,
    pub staticness: Staticness,
    pub kind: ImportKind,
    /// Inline `mod` nesting within the file that contains the site (Rust only).
    pub scope: Vec<String>,
    /// Imported name for named imports and `use` leaves; `default` and `*` for TS defaults and namespaces.
    pub imported_name: Option<String>,
    /// Name the import is bound to when it differs from `imported_name`.
    pub alias: Option<String>,
    pub glob: bool,
    pub reexport: bool,
    /// False when a dynamic specifier is not a string literal.
    pub literal_specifier: bool,
    pub path_attribute: PathAttribute,
}

/// A top-level declaration (or export) a file offers to importers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeclaredItem {
    pub name: String,
    pub kind: ItemKind,
    pub visibility: Visibility,
    pub span: ByteSpan,
    pub scope: Vec<String>,
}

/// Everything an adapter learned from one file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtractedFile {
    pub facts: Vec<ImportFact>,
    pub items: Vec<DeclaredItem>,
    pub diagnostics: Vec<String>,
    /// True when `max_facts_per_file` stopped extraction early.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Found,
    NotFound,
    NotAttempted,
}

/// Why an import could not be tied to a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportReason {
    PathNotFound,
    UndeclaredExternal,
    AliasUnsupported,
    SubpathUnsupported,
    NonLiteralSpecifier,
    PathAttributeUnsupported,
    UnsupportedForm,
    UnattachedFile,
    HopLimitExceeded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReexportHop {
    pub file: RootRelativePath,
    pub fact: ImportFactId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    ResolvedInternal {
        target_file: RootRelativePath,
        target_item: Option<String>,
        item_status: ItemStatus,
        /// Re-exports followed from `target_file` to reach the declaration.
        via: Vec<ReexportHop>,
        /// File that declares the item when it differs from `target_file`.
        definition_file: Option<RootRelativePath>,
    },
    DeclaredExternal {
        package: String,
    },
    Possible {
        candidates: Vec<RootRelativePath>,
        truncated: bool,
    },
    Unresolved {
        reason: ImportReason,
    },
    DynamicOrUnsupported {
        reason: ImportReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    ResolvedInternal,
    DeclaredExternal,
    Possible,
    Unresolved,
    DynamicOrUnsupported,
}

impl Resolution {
    pub fn kind(&self) -> ResolutionKind {
        match self {
            Self::ResolvedInternal { .. } => ResolutionKind::ResolvedInternal,
            Self::DeclaredExternal { .. } => ResolutionKind::DeclaredExternal,
            Self::Possible { .. } => ResolutionKind::Possible,
            Self::Unresolved { .. } => ResolutionKind::Unresolved,
            Self::DynamicOrUnsupported { .. } => ResolutionKind::DynamicOrUnsupported,
        }
    }

    pub fn target_file(&self) -> Option<&RootRelativePath> {
        match self {
            Self::ResolvedInternal { target_file, .. } => Some(target_file),
            _ => None,
        }
    }
}

/// A fact paired with the resolver's conclusion and the rule that produced it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportEdge {
    pub fact: ImportFact,
    pub from_package: Option<PackageId>,
    pub to_package: Option<PackageId>,
    pub resolution: Resolution,
    pub rule: String,
    /// Config or manifest files whose contents supported this resolution (for example a `tsconfig.json`).
    pub evidence: Vec<RootRelativePath>,
}

/// Resolver output for one fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedImport {
    pub fact: ImportFactId,
    pub resolution: Resolution,
    pub rule: String,
    pub evidence: Vec<RootRelativePath>,
}

/// Explicit ceilings for extraction, resolution, and paging.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportLimits {
    pub max_file_bytes: u64,
    pub max_facts_per_file: usize,
    pub max_total_facts: usize,
    pub max_candidates: usize,
    pub max_reexport_hops: usize,
    pub max_page_limit: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024,
            max_facts_per_file: 10_000,
            max_total_facts: 1_000_000,
            max_candidates: 8,
            max_reexport_hops: 8,
            max_page_limit: 500,
        }
    }
}

/// Parses one language's source into import facts. Must not execute or fetch anything.
pub trait LanguageImportAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    /// Bump whenever extraction output could change for the same input.
    fn version(&self) -> u32;
    /// The profile this adapter extracts for `path`, or `None` when it does not handle it.
    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile>;
    fn forms(&self) -> Vec<ImportForm>;
    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile;
}

/// An extracted file placed in its package.
pub struct SourceUnit {
    pub path: RootRelativePath,
    pub package: Option<PackageId>,
    pub profile: LanguageProfile,
    pub extracted: std::sync::Arc<ExtractedFile>,
}

/// A compiler config file (for example `tsconfig.json`) loaded for resolvers; resolvers never read the disk.
pub struct ConfigFile {
    pub path: RootRelativePath,
    pub text: String,
}

pub struct ResolveContext<'a> {
    pub root: &'a Path,
    pub structure: &'a RepositoryStructure,
    /// Compiler configs inventoried by discovery, sorted by path.
    pub configs: &'a [ConfigFile],
    /// Units for every profile of every registered adapter, sorted by path.
    pub units: &'a [SourceUnit],
    pub limits: &'a ImportLimits,
    pub cancel: &'a CancellationToken,
}

#[derive(Default)]
pub struct ResolveOutput {
    pub resolved: Vec<ResolvedImport>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    /// Source files the resolver could not attach to any root (for example Rust files no
    /// Cargo target reaches); kept as data so consumers need not parse diagnostics.
    pub unattached: Vec<RootRelativePath>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportBuildError {
    #[error("import resolution cancelled")]
    Cancelled,
    #[error("import resolution failed: {0}")]
    Failed(String),
}

/// Ties facts to targets using static rules only.
pub trait ImportResolver: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn profiles(&self) -> Vec<LanguageProfile>;
    /// Names of the rules this resolver can cite as evidence.
    fn rules(&self) -> Vec<&'static str>;
    /// Forms and conventions this resolver deliberately does not handle.
    fn unsupported(&self) -> Vec<&'static str>;
    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError>;
}

/// What one registered language can and cannot do; text must match tested behavior.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LanguageCapability {
    pub profiles: Vec<LanguageProfile>,
    pub adapter: String,
    pub adapter_version: u32,
    pub resolver: String,
    pub resolver_version: u32,
    pub forms: Vec<ImportForm>,
    pub resolution_rules: Vec<String>,
    pub unsupported: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportCapabilities {
    pub languages: Vec<LanguageCapability>,
    pub limits: ImportLimits,
}
