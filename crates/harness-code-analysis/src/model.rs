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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallableSymbol {
    pub id: SymbolId,
    pub name: String,
    pub language: LanguageId,
    pub span: SourceSpan,
    pub owner: Option<SymbolId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallSite {
    pub caller: SymbolId,
    pub span: SourceSpan,
    pub target: Option<SymbolId>,
    pub resolution: Resolution,
    pub evidence: Option<Evidence>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub metadata: QueryMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshRequest {
    pub max_files: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshResponse {
    pub snapshot: SnapshotId,
    pub metadata: QueryMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindCallablesRequest {
    pub snapshot: Option<SnapshotId>,
    pub language: Option<LanguageId>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallableResponse {
    pub page: Page<CallableSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallsFromRequest {
    pub snapshot: SnapshotId,
    pub caller: SymbolId,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallsToRequest {
    pub snapshot: SnapshotId,
    pub target: SymbolId,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallsResponse {
    pub page: Page<CallSite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraversalRequest {
    pub snapshot: SnapshotId,
    pub roots: Vec<SymbolId>,
    pub depth: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraversalResponse {
    pub nodes: Page<CallableSymbol>,
    pub edges: Page<CallSite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageCapabilities {
    pub language: LanguageId,
    pub supported: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallGraphCapabilities {
    pub languages: Vec<LanguageCapabilities>,
    pub max_page_size: u32,
    pub max_traversal_depth: u32,
    pub supports_partial_results: bool,
    pub supports_cross_file_resolution: bool,
}
