use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub String);

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LanguageId {
    JavaScript,
    TypeScript,
    Python,
    Java,
    CSharp,
    Go,
    Rust,
    Php,
    Cpp,
    Ruby,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSpan {
    pub path: String,
    pub start_byte: u32,
    pub end_byte: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CallableKind {
    #[default]
    Function,
    Method,
    Constructor,
    Closure,
    /// Module, class, or other initialization code that owns calls outside any named callable.
    Synthetic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallableSymbol {
    pub id: SymbolId,
    pub name: String,
    #[serde(default)]
    pub qualified_name: String,
    #[serde(default)]
    pub kind: CallableKind,
    pub language: LanguageId,
    pub span: SourceSpan,
    pub owner: Option<SymbolId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Resolution {
    Resolved,
    Possible,
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub rule_id: String,
    pub adapter_version: String,
    pub explanation: String,
    /// Source location supporting the rule, such as the import statement used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<SourceSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallSite {
    pub caller: SymbolId,
    pub span: SourceSpan,
    pub target: Option<SymbolId>,
    pub resolution: Resolution,
    pub evidence: Option<Evidence>,
    /// Remaining workspace targets when the resolution is `Possible`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<SymbolId>,
    /// True when more candidates existed than the candidate limit allows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub candidates_truncated: bool,
    /// Explicitly identified target outside the workspace (for example a standard-library or
    /// third-party module); it is never a workspace definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryMetadata {
    pub snapshot: SnapshotId,
    pub partial: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub truncated: bool,
    pub limits: Vec<String>,
    pub freshness: String,
    /// Unix time (seconds) at which the generation was published. The workspace was not frozen
    /// while it was built; per-file hashes identify the exact bytes analysed.
    #[serde(default)]
    pub generated_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub metadata: QueryMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    #[serde(default)]
    pub max_files: Option<u32>,
}

/// Aggregate counters for one refresh. Contains no source text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RefreshStats {
    pub files_discovered: u32,
    /// Files handed to a language adapter (cache hits included).
    pub files_analyzed: u32,
    /// Discovered files without a registered adapter or that failed analysis.
    pub files_skipped: u32,
    pub source_bytes: u64,
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub callables: u32,
    pub calls: u32,
    pub resolved: u32,
    pub possible: u32,
    pub unresolved: u32,
    pub unresolved_by_reason: BTreeMap<String, u32>,
    /// Number of discovery or analysis limits that omitted input.
    pub limits_triggered: u32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshResponse {
    pub snapshot: SnapshotId,
    pub metadata: QueryMetadata,
    #[serde(default)]
    pub stats: RefreshStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FindCallablesRequest {
    #[serde(default)]
    pub snapshot: Option<SnapshotId>,
    #[serde(default)]
    pub language: Option<LanguageId>,
    /// Matches the simple name exactly, or the qualified name exactly or as a dotted suffix.
    #[serde(default)]
    pub name: Option<String>,
    /// Restricts results to one workspace-relative file.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallableResponse {
    pub page: Page<CallableSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallsFromRequest {
    pub snapshot: SnapshotId,
    pub caller: SymbolId,
    /// Restricts results to these resolutions; empty means all.
    #[serde(default)]
    pub resolutions: Vec<Resolution>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallsToRequest {
    pub snapshot: SnapshotId,
    pub target: SymbolId,
    /// Restricts results to these resolutions; empty means resolved and possible edges.
    #[serde(default)]
    pub resolutions: Vec<Resolution>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallsResponse {
    pub page: Page<CallSite>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow calls made by the roots.
    #[default]
    Callees,
    /// Follow calls that reach the roots.
    Callers,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraversalRequest {
    pub snapshot: SnapshotId,
    pub roots: Vec<SymbolId>,
    pub depth: u32,
    pub limit: u32,
    #[serde(default)]
    pub direction: Direction,
    /// Restricts followed edges to these resolutions; empty means resolved and possible edges.
    #[serde(default)]
    pub resolutions: Vec<Resolution>,
}

/// The bound responsible for an incomplete traversal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraversalLimit {
    /// Edges leaving nodes at the requested depth were not followed.
    Depth,
    /// More nodes were reached than the page limit allows.
    NodeLimit,
    /// More edges were found than the page limit allows.
    EdgeLimit,
    /// The internal visit budget stopped the walk.
    VisitBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraversalResponse {
    pub nodes: Page<CallableSymbol>,
    pub edges: Page<CallSite>,
    /// Every bound that cut the subgraph short; empty when it is complete for the request.
    #[serde(default)]
    pub truncated_by: Vec<TraversalLimit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageCapabilities {
    pub language: LanguageId,
    pub supported: bool,
    pub limitations: Vec<String>,
    /// Extraction adapter version; part of every cache identity and snapshot identity.
    #[serde(default)]
    pub adapter_version: String,
    /// Resolution rule-set version reported in call-site evidence.
    #[serde(default)]
    pub resolver_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallGraphCapabilities {
    pub languages: Vec<LanguageCapabilities>,
    pub max_page_size: u32,
    pub max_traversal_depth: u32,
    pub supports_partial_results: bool,
    pub supports_cross_file_resolution: bool,
}
