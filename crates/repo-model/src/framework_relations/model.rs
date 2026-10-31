//! Framework relations: facts about conventions that imports do not show.
//!
//! A relation says "this route is served by that function", "this provider is injected there",
//! "this component renders that one". Each one carries the rule and version that produced it,
//! the source text that justifies it, and whether the source states it outright or a naming or
//! directory convention implies it. Relations live beside the import index and never become
//! import edges.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    import_graph::{ByteSpan, ComponentVersion},
    repository_intelligence::{DiscoveryDiagnostic, PackageId, RootRelativePath},
};

/// What a relation means. Kinds are shared across frameworks so tools and the canvas can treat
/// "a route" the same whether Express or Axum declared it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// An HTTP route served by a handler.
    RouteRegisteredTo,
    /// A provider, bean or service injected where it is consumed.
    ProviderInjectedInto,
    /// A component used by another component.
    ComponentRenders,
    /// A function exposed as a callable command.
    CommandExposedAs,
    /// An ORM model tied to a table name.
    ModelMappedToTable,
}

impl RelationKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::RouteRegisteredTo => "an HTTP route served by a handler",
            Self::ProviderInjectedInto => "a provider injected where it is consumed",
            Self::ComponentRenders => "a component rendered by another component",
            Self::CommandExposedAs => "a function exposed as a callable command",
            Self::ModelMappedToTable => "a model mapped to a table",
        }
    }
}

/// How firmly the source supports a relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Certainty {
    /// Written in the code itself: a literal route, a decorator, a registration call.
    Declared,
    /// Follows from a documented naming or directory convention (for example file-system routes).
    Convention,
}

/// Deterministic identity of a relation.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct RelationId(pub String);

/// The source text behind a relation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelationEvidence {
    pub file: RootRelativePath,
    pub span: ByteSpan,
    /// The literal source, bounded to [`EVIDENCE_TEXT_LIMIT`] characters.
    pub text: String,
}

/// The longest evidence text kept; longer source is cut and marked with an ellipsis.
pub const EVIDENCE_TEXT_LIMIT: usize = 160;

impl RelationEvidence {
    pub fn new(file: &RootRelativePath, span: ByteSpan, text: &str) -> Self {
        let mut shown = text.trim().replace('\n', " ");
        if shown.chars().count() > EVIDENCE_TEXT_LIMIT {
            shown = shown.chars().take(EVIDENCE_TEXT_LIMIT).collect::<String>() + "…";
        }
        Self {
            file: file.clone(),
            span,
            text: shown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkRelation {
    pub id: RelationId,
    pub kind: RelationKind,
    /// The framework's id, for example `express`.
    pub framework: String,
    /// The rule that produced the relation, with its version.
    pub rule: ComponentVersion,
    pub certainty: Certainty,
    /// The file that declares or registers the relation.
    pub from: RootRelativePath,
    /// The file the relation points at; the same file for a route handled where it is registered,
    /// and `None` when the other end is not an indexed file (a built-in or external handler).
    pub to: Option<RootRelativePath>,
    pub from_symbol: Option<String>,
    pub to_symbol: Option<String>,
    /// What the relation is about: `GET /users`, a command name, a selector, a token.
    pub label: String,
    pub evidence: RelationEvidence,
    pub package: Option<PackageId>,
}

/// Everything an adapter decides about one relation; the identity is derived from it.
pub struct RelationDraft {
    pub kind: RelationKind,
    pub rule: &'static str,
    pub rule_version: u32,
    pub certainty: Certainty,
    pub from: RootRelativePath,
    pub to: Option<RootRelativePath>,
    pub from_symbol: Option<String>,
    pub to_symbol: Option<String>,
    pub label: String,
    pub evidence: RelationEvidence,
}

impl FrameworkRelation {
    pub fn new(framework: &str, package: Option<&PackageId>, draft: RelationDraft) -> Self {
        let identity = format!(
            "{framework}\u{0}{}\u{0}{:?}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            draft.rule,
            draft.kind,
            draft.from.as_path().display(),
            draft.evidence.span.start,
            draft.label,
            draft
                .to
                .as_ref()
                .map(|to| to.as_path().display().to_string())
                .unwrap_or_default(),
        );
        let id = format!("{:x}", Sha256::digest(identity.as_bytes()));
        Self {
            id: RelationId(format!("rel:{}", &id[..16])),
            kind: draft.kind,
            framework: framework.to_owned(),
            rule: ComponentVersion {
                id: draft.rule.to_owned(),
                version: draft.rule_version,
            },
            certainty: draft.certainty,
            from: draft.from,
            to: draft.to,
            from_symbol: draft.from_symbol,
            to_symbol: draft.to_symbol,
            label: draft.label,
            evidence: draft.evidence,
            package: package.cloned(),
        }
    }
}

/// What one adapter detects and what it deliberately leaves alone; the text must match tested behaviour.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkCapability {
    pub framework: String,
    pub name: String,
    pub adapter_version: u32,
    pub kinds: Vec<RelationKind>,
    /// Forms the adapter reads, in plain words.
    pub handled: Vec<String>,
    /// Forms it does not read; where they appear it reports a diagnostic and no relation.
    pub unsupported: Vec<String>,
}

/// A framework found active in a package, and why.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FrameworkApplication {
    pub framework: String,
    pub package: PackageId,
    /// The manifest fact that made the adapter applicable, such as a declared dependency.
    pub evidence: String,
}

/// Explicit ceilings for framework analysis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkLimits {
    pub max_file_bytes: u64,
    pub max_files_per_package: usize,
    pub max_relations_per_file: usize,
    pub max_relations: usize,
    pub max_page_limit: usize,
}

impl Default for FrameworkLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1024 * 1024,
            max_files_per_package: 20_000,
            max_relations_per_file: 200,
            max_relations: 50_000,
            max_page_limit: 500,
        }
    }
}

/// The immutable set of framework relations stored in a snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameworkIndex {
    /// True when an analyzer ran; queries on an unconfigured index fail instead of returning nothing.
    pub configured: bool,
    /// Sorted by file, position, then id.
    pub relations: Vec<FrameworkRelation>,
    /// Conventions that could not be read statically: reported, never turned into relations.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    /// Frameworks found active, by package. A framework absent here was off for that package.
    pub applications: Vec<FrameworkApplication>,
    pub capabilities: Vec<FrameworkCapability>,
    /// True when a limit left some relations out.
    pub partial: bool,
    pub limits: FrameworkLimits,
    /// Digest of everything above; folded into the snapshot id.
    pub digest: String,
}

/// Everything a [`FrameworkIndex`] is built from; `FrameworkIndex::new` sorts it into canonical order.
pub struct FrameworkIndexParts {
    pub relations: Vec<FrameworkRelation>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub applications: Vec<FrameworkApplication>,
    pub capabilities: Vec<FrameworkCapability>,
    pub partial: bool,
    pub limits: FrameworkLimits,
}

impl FrameworkIndex {
    pub fn new(parts: FrameworkIndexParts) -> Self {
        let FrameworkIndexParts {
            mut relations,
            mut diagnostics,
            mut applications,
            mut capabilities,
            partial,
            limits,
        } = parts;
        relations.sort_by(|a, b| {
            (&a.from, a.evidence.span.start, &a.id).cmp(&(&b.from, b.evidence.span.start, &b.id))
        });
        relations.dedup_by(|a, b| a.id == b.id);
        diagnostics.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
        diagnostics.dedup();
        applications.sort();
        applications.dedup();
        capabilities.sort_by(|a, b| a.framework.cmp(&b.framework));
        let mut index = Self {
            configured: true,
            relations,
            diagnostics,
            applications,
            capabilities,
            partial,
            limits,
            digest: String::new(),
        };
        index.digest = index.compute_digest();
        index
    }

    /// The adapters that contributed, as versioned components (for result envelopes).
    pub fn components(&self) -> Vec<ComponentVersion> {
        self.capabilities
            .iter()
            .map(|capability| ComponentVersion {
                id: format!("framework:{}", capability.framework),
                version: capability.adapter_version,
            })
            .collect()
    }

    fn compute_digest(&self) -> String {
        let mut view = self.clone();
        view.digest.clear();
        let bytes = serde_json::to_vec(&view).unwrap_or_default();
        format!("{:x}", Sha256::digest(bytes))
    }
}
