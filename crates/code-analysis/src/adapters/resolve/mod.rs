//! Language-aware call resolution.
//!
//! [`LanguageResolver`] is the public contract. The built-in resolvers share one engine
//! ([`SchemeResolver`]) that implements lexical shadowing, member lookup, constructor lookup and
//! candidate handling once, while each language supplies a [`Scheme`] describing how names become
//! visible: which module an import denotes, which files share a package scope, and so on. No scheme
//! resolves by bare-name matching across the workspace.

mod calls;
mod lookup;
mod receivers;

use std::collections::{BTreeMap, BTreeSet};

use super::facts::*;
use crate::model::{LanguageId, SourceSpan};

/// A workspace callable identified by file and per-file key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TargetRef {
    pub path: String,
    pub key: u32,
}

/// What a resolver concluded about one call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Resolved {
        target: TargetRef,
        rule: &'static str,
        explanation: String,
        span: Option<SourceSpan>,
    },
    Possible {
        targets: Vec<TargetRef>,
        rule: &'static str,
        explanation: String,
        span: Option<SourceSpan>,
    },
    Unresolved {
        rule: &'static str,
        explanation: String,
    },
    /// The callee was positively identified as living outside the workspace.
    External {
        name: String,
        rule: &'static str,
        explanation: String,
        span: Option<SourceSpan>,
    },
}

/// Read-only view of every extracted file in one analysis generation.
pub struct ProjectIndex<'a> {
    files: BTreeMap<&'a str, &'a ExtractedFile>,
}

impl<'a> ProjectIndex<'a> {
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a ExtractedFile)>) -> Self {
        Self {
            files: files.into_iter().collect(),
        }
    }

    pub fn file(&self, path: &str) -> Option<&'a ExtractedFile> {
        self.files.get(path).copied()
    }

    pub fn paths(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.files.keys().copied()
    }

    pub fn files(&self) -> impl Iterator<Item = (&'a str, &'a ExtractedFile)> + '_ {
        self.files.iter().map(|(path, file)| (*path, *file))
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }
}

/// Everything a resolver may consult for one call.
pub struct ResolutionContext<'a> {
    pub path: &'a str,
    pub file: &'a ExtractedFile,
    pub project: &'a ProjectIndex<'a>,
    /// Namespace, package or module in effect at the call site.
    pub namespace: &'a str,
}

/// Language-specific resolution contract.
pub trait LanguageResolver: Send + Sync {
    fn language(&self) -> LanguageId;
    fn version(&self) -> &'static str;
    fn resolve(&self, ctx: &ResolutionContext<'_>, call: &CallFact) -> Outcome;
}

/// A name brought into scope by an import.
#[derive(Clone, Debug)]
pub(crate) struct ImportHit {
    pub path: String,
    /// Member name inside the target (differs from the local name for aliases).
    pub member: String,
    /// When set, `member` is a method/associated function of this type in `path`.
    pub via_type: Option<String>,
    /// Restricts the lookup to declarations in this namespace/module.
    pub namespace: Option<String>,
    pub rule: &'static str,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ImportLookup {
    pub hits: Vec<ImportHit>,
    /// Set when the import explicitly denotes a target outside the workspace.
    pub external: Option<(String, SourceSpan)>,
}

/// A file whose top-level declarations are visible without qualification.
#[derive(Clone, Debug)]
pub(crate) struct Peer {
    pub path: String,
    /// Restricts visible declarations to this namespace when set.
    pub namespace: Option<String>,
    /// Whether free functions (not only types) of the file are visible.
    pub functions: bool,
    pub rule: &'static str,
    pub span: Option<SourceSpan>,
}

/// A module/namespace bound to a local alias.
#[derive(Clone, Debug)]
pub(crate) struct ModuleHit {
    pub path: String,
    pub namespace: Option<String>,
    pub rule: &'static str,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModuleLookup {
    pub hits: Vec<ModuleHit>,
    pub external: Option<(String, SourceSpan)>,
}

/// How a language makes names visible. All hooks default to "nothing".
pub(crate) trait Scheme: Send + Sync {
    fn language(&self) -> LanguageId;
    fn version(&self) -> &'static str {
        "resolve-1"
    }
    /// Bare calls inside a method may denote methods of the enclosing type.
    fn bare_call_sees_enclosing_type(&self) -> bool {
        false
    }
    /// What `Name(...)` means when `Name` is a visible type.
    fn bare_type_call(&self) -> BareTypeCall {
        BareTypeCall::Nothing
    }
    /// Classes may be reopened elsewhere, so same-file definitions never rule out peers.
    fn types_may_reopen(&self) -> bool {
        false
    }
    /// Files that may hold members (out-of-line definitions) of a type declared in `path`.
    fn related_files(&self, _ctx: &ResolutionContext<'_>, path: &str) -> Vec<String> {
        vec![path.to_owned()]
    }
    fn named_import(&self, _ctx: &ResolutionContext<'_>, _name: &str) -> ImportLookup {
        ImportLookup::default()
    }
    fn module_alias(&self, _ctx: &ResolutionContext<'_>, _path: &[String]) -> ModuleLookup {
        ModuleLookup::default()
    }
    fn peers(&self, _ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BareTypeCall {
    /// `Name(...)` is an ordinary call.
    Nothing,
    /// `Name(...)` constructs a `Name` (Python, C++).
    Construct,
    /// `Name(...)` converts a value to `Name` (Go); it is not a call to workspace code.
    Conversion,
}

pub(crate) struct SchemeResolver<S: Scheme>(pub S);

impl<S: Scheme> LanguageResolver for SchemeResolver<S> {
    fn language(&self) -> LanguageId {
        self.0.language()
    }

    fn version(&self) -> &'static str {
        self.0.version()
    }

    fn resolve(&self, ctx: &ResolutionContext<'_>, call: &CallFact) -> Outcome {
        Engine {
            scheme: &self.0,
            ctx,
            call,
        }
        .resolve()
    }
}

/// Resolver used when a custom adapter has no matching resolver: everything stays unresolved.
pub(crate) struct UnresolvedResolver(pub LanguageId);

impl LanguageResolver for UnresolvedResolver {
    fn language(&self) -> LanguageId {
        self.0.clone()
    }

    fn version(&self) -> &'static str {
        "none"
    }

    fn resolve(&self, _ctx: &ResolutionContext<'_>, _call: &CallFact) -> Outcome {
        Outcome::Unresolved {
            rule: "no-resolver",
            explanation: "no resolver is registered for this language".into(),
        }
    }
}

const MAX_BASE_DEPTH: usize = 8;

struct Engine<'a, S: Scheme> {
    scheme: &'a S,
    ctx: &'a ResolutionContext<'a>,
    call: &'a CallFact,
}

#[derive(Clone)]
struct TypeHit {
    path: String,
    ty: TypeFact,
    rule: &'static str,
    span: Option<SourceSpan>,
}

fn unresolved(rule: &'static str, explanation: impl Into<String>) -> Outcome {
    Outcome::Unresolved {
        rule,
        explanation: explanation.into(),
    }
}

fn span_of(path: &str, start: u32, end: u32) -> SourceSpan {
    SourceSpan {
        path: path.to_owned(),
        start_byte: start,
        end_byte: end,
    }
}

fn ns_visible(call_ns: &str, target_ns: &str) -> bool {
    target_ns.is_empty()
        || call_ns == target_ns
        || ["::", "\\", "."]
            .iter()
            .any(|sep| call_ns.starts_with(&format!("{target_ns}{sep}")))
}

fn ns_matches_path(target_ns: &str, prefix: &[String]) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let normalized = target_ns.replace(['\\', ':'], ".");
    let joined = prefix.join(".");
    let normalized = normalized.trim_matches('.').replace("..", ".");
    normalized == joined || normalized.ends_with(&format!(".{joined}"))
}

impl<S: Scheme> Engine<'_, S> {
    fn resolve(&self) -> Outcome {
        let file = self.ctx.file;
        let caller_range = self
            .caller()
            .map(|caller| (caller.node_start, caller.node_end))
            .unwrap_or((self.call.start, self.call.end));
        if file.overlaps_error(self.call.start, self.call.end)
            || file.overlaps_error(caller_range.0, caller_range.1)
        {
            return unresolved(
                "syntax-error-affected",
                "the call or its owner overlaps a syntax error; exact resolution is withheld",
            );
        }
        match self.call.kind {
            CallKind::Dynamic => unresolved(
                "dynamic-callee",
                "the callee is computed at runtime and has no static name",
            ),
            CallKind::Function => self.function(),
            CallKind::Constructor => self.constructor(),
            CallKind::Method | CallKind::Static => self.member(),
        }
    }

    // ----- lexical lookup -----
}

struct VisibleTypes {
    hits: Vec<TypeHit>,
    external: Option<(String, SourceSpan)>,
}

enum Lexical {
    None,
    Callable(u32),
    Value,
}

fn dedupe_types(hits: &mut Vec<TypeHit>) {
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.ty.key)));
}

// ----- path helpers shared by schemes -----

/// Directory part of a root-relative path (`""` for root files).
pub(crate) fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Joins `relative` onto `base_dir`, resolving `.` and `..`; `None` if it leaves the root.
pub(crate) fn join_normalized(base_dir: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn project_span(path: &str, start: u32, end: u32) -> SourceSpan {
    span_of(path, start, end)
}
