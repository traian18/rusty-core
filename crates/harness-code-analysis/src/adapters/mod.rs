mod collector;
mod facts;
mod lang;
mod resolve;

use std::path::Path;
use std::sync::Arc;

use crate::api::CallGraphError;
use crate::model::{LanguageCapabilities, LanguageId};

pub use facts::{
    CallFact, CallKind, CallableFact, ExportFact, ExtractedFile, ImportFact, ImportKind,
    LocalBinding, Receiver, TypeFact, TypeKind,
};
pub use resolve::{LanguageResolver, Outcome, ProjectIndex, ResolutionContext, TargetRef};

use collector::Syntax;
use resolve::UnresolvedResolver;

/// Extracts per-file facts for one language.
pub trait LanguageAdapter: Send + Sync {
    fn language(&self) -> LanguageId;
    /// Changing this invalidates cached extraction facts.
    fn version(&self) -> &'static str;
    fn capabilities(&self) -> LanguageCapabilities;
    fn extract(&self, path: &str, source: &str) -> Result<ExtractedFile, CallGraphError>;
}

pub(crate) struct AdapterRegistry {
    adapters: Vec<Arc<dyn LanguageAdapter>>,
    resolvers: Vec<Arc<dyn LanguageResolver>>,
}

impl AdapterRegistry {
    pub(crate) fn new(
        adapters: Vec<Arc<dyn LanguageAdapter>>,
        mut resolvers: Vec<Arc<dyn LanguageResolver>>,
    ) -> Result<Self, CallGraphError> {
        if adapters.is_empty() {
            return Err(CallGraphError::InvalidRequest(
                "at least one adapter is required".into(),
            ));
        }
        for (index, adapter) in adapters.iter().enumerate() {
            if adapters[..index]
                .iter()
                .any(|candidate| candidate.language() == adapter.language())
            {
                return Err(CallGraphError::InvalidRequest(
                    "duplicate language adapter".into(),
                ));
            }
        }
        for (index, resolver) in resolvers.iter().enumerate() {
            if resolvers[..index]
                .iter()
                .any(|candidate| candidate.language() == resolver.language())
            {
                return Err(CallGraphError::InvalidRequest(
                    "duplicate language resolver".into(),
                ));
            }
            if !adapters
                .iter()
                .any(|adapter| adapter.language() == resolver.language())
            {
                return Err(CallGraphError::InvalidRequest(
                    "resolver has no adapter for its language".into(),
                ));
            }
        }
        for adapter in &adapters {
            if !resolvers
                .iter()
                .any(|resolver| resolver.language() == adapter.language())
            {
                resolvers.push(Arc::new(UnresolvedResolver(adapter.language())));
            }
        }
        Ok(Self {
            adapters,
            resolvers,
        })
    }

    pub(crate) fn builtin() -> Self {
        let mut adapters: Vec<Arc<dyn LanguageAdapter>> = Vec::new();
        let mut resolvers: Vec<Arc<dyn LanguageResolver>> = Vec::new();
        for language in ALL_LANGUAGES {
            adapters.push(Arc::new(TreeSitterAdapter {
                syntax: lang::syntax_for(&language),
            }));
            resolvers.push(lang::resolver_for(&language));
        }
        Self {
            adapters,
            resolvers,
        }
    }

    pub(crate) fn adapter_for_path(&self, path: &str) -> Option<&dyn LanguageAdapter> {
        let language = language_for(path)?;
        self.adapter_for(&language)
    }

    pub(crate) fn adapter_for(&self, language: &LanguageId) -> Option<&dyn LanguageAdapter> {
        self.adapters
            .iter()
            .find(|adapter| &adapter.language() == language)
            .map(AsRef::as_ref)
    }

    pub(crate) fn resolver_for(&self, language: &LanguageId) -> Option<&dyn LanguageResolver> {
        self.resolvers
            .iter()
            .find(|resolver| &resolver.language() == language)
            .map(AsRef::as_ref)
    }

    pub(crate) fn capabilities(&self) -> Vec<LanguageCapabilities> {
        self.adapters
            .iter()
            .map(|adapter| {
                let mut capabilities = adapter.capabilities();
                capabilities.adapter_version = adapter.version().to_owned();
                capabilities.resolver_version = self
                    .resolver_for(&adapter.language())
                    .map_or_else(String::new, |resolver| resolver.version().to_owned());
                capabilities
            })
            .collect()
    }
}

pub(crate) const ALL_LANGUAGES: [LanguageId; 10] = [
    LanguageId::JavaScript,
    LanguageId::TypeScript,
    LanguageId::Python,
    LanguageId::Java,
    LanguageId::CSharp,
    LanguageId::Go,
    LanguageId::Rust,
    LanguageId::Php,
    LanguageId::Cpp,
    LanguageId::Ruby,
];

struct TreeSitterAdapter {
    syntax: Box<dyn Syntax>,
}

impl LanguageAdapter for TreeSitterAdapter {
    fn language(&self) -> LanguageId {
        self.syntax.language()
    }

    fn version(&self) -> &'static str {
        "tree-sitter-2"
    }

    fn capabilities(&self) -> LanguageCapabilities {
        LanguageCapabilities {
            language: self.language(),
            supported: true,
            limitations: self.syntax.limitations(),
            adapter_version: String::new(),
            resolver_version: String::new(),
        }
    }

    fn extract(&self, path: &str, source: &str) -> Result<ExtractedFile, CallGraphError> {
        collector::extract(self.syntax.as_ref(), path, source)
    }
}

pub(crate) fn language_for(path: &str) -> Option<LanguageId> {
    Some(match Path::new(path).extension()?.to_str()? {
        "js" | "jsx" | "mjs" | "cjs" => LanguageId::JavaScript,
        "ts" | "tsx" | "mts" | "cts" => LanguageId::TypeScript,
        "py" => LanguageId::Python,
        "java" => LanguageId::Java,
        "cs" => LanguageId::CSharp,
        "go" => LanguageId::Go,
        "rs" => LanguageId::Rust,
        "php" => LanguageId::Php,
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => LanguageId::Cpp,
        "rb" => LanguageId::Ruby,
        _ => return None,
    })
}
