//! Language-aware call resolution.
//!
//! [`LanguageResolver`] is the public contract. The built-in resolvers share one engine
//! ([`SchemeResolver`]) that implements lexical shadowing, member lookup, constructor lookup and
//! candidate handling once, while each language supplies a [`Scheme`] describing how names become
//! visible: which module an import denotes, which files share a package scope, and so on. No scheme
//! resolves by bare-name matching across the workspace.

use std::collections::{BTreeMap, BTreeSet};

use super::facts::*;
use crate::model::{CallableKind, LanguageId, SourceSpan};

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
    fn caller(&self) -> Option<&CallableFact> {
        self.ctx.file.callable(self.call.caller)
    }

    fn call_namespace(&self) -> String {
        self.ctx.namespace.to_owned()
    }

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

    fn lexical(&self, name: &str) -> Lexical {
        let file = self.ctx.file;
        let mut current = file.callable(self.call.caller);
        while let Some(callable) = current {
            if let Some(binding) = callable.locals.iter().find(|item| item.name == name) {
                return match binding.callable {
                    Some(key) => Lexical::Callable(key),
                    None => Lexical::Value,
                };
            }
            current = callable.parent.and_then(|parent| file.callable(parent));
        }
        match file.module_locals.iter().find(|item| item.name == name) {
            Some(binding) => match binding.callable {
                Some(key) => Lexical::Callable(key),
                None => Lexical::Value,
            },
            None => Lexical::None,
        }
    }

    fn local_binding(&self, name: &str) -> Option<LocalBinding> {
        let file = self.ctx.file;
        let mut current = file.callable(self.call.caller);
        while let Some(callable) = current {
            if let Some(binding) = callable.locals.iter().find(|item| item.name == name) {
                return Some(binding.clone());
            }
            current = callable.parent.and_then(|parent| file.callable(parent));
        }
        file.module_locals
            .iter()
            .find(|item| item.name == name)
            .cloned()
    }

    // ----- declaration lookup -----

    fn free_functions(&self, path: &str, name: &str, namespace: Option<&str>) -> Vec<TargetRef> {
        let Some(file) = self.ctx.project.file(path) else {
            return Vec::new();
        };
        let call_ns = self.call_namespace();
        file.callables
            .iter()
            .filter(|item| {
                item.parent.is_none()
                    && item.container.is_none()
                    && matches!(item.kind, CallableKind::Function | CallableKind::Closure)
                    && item.name == name
                    && match namespace {
                        Some(ns) => item.namespace == ns,
                        None if path == self.ctx.path => ns_visible(&call_ns, &item.namespace),
                        None => true,
                    }
            })
            .map(|item| TargetRef {
                path: path.to_owned(),
                key: item.key,
            })
            .collect()
    }

    fn methods(&self, path: &str, ty: &TypeFact, name: &str) -> Vec<TargetRef> {
        let mut out = Vec::new();
        for related in self.scheme.related_files(self.ctx, path) {
            let Some(file) = self.ctx.project.file(&related) else {
                continue;
            };
            out.extend(
                file.callables
                    .iter()
                    .filter(|item| {
                        item.parent.is_none()
                            && item.container.as_deref() == Some(ty.name.as_str())
                            && item.namespace == ty.namespace
                            && matches!(
                                item.kind,
                                CallableKind::Function
                                    | CallableKind::Method
                                    | CallableKind::Constructor
                            )
                            && item.name == name
                    })
                    .map(|item| TargetRef {
                        path: related.clone(),
                        key: item.key,
                    }),
            );
        }
        out
    }

    fn constructors(&self, path: &str, ty: &TypeFact) -> Vec<TargetRef> {
        let mut out = Vec::new();
        for related in self.scheme.related_files(self.ctx, path) {
            let Some(file) = self.ctx.project.file(&related) else {
                continue;
            };
            out.extend(
                file.callables
                    .iter()
                    .filter(|item| {
                        item.parent.is_none()
                            && item.container.as_deref() == Some(ty.name.as_str())
                            && item.namespace == ty.namespace
                            && item.kind == CallableKind::Constructor
                    })
                    .map(|item| TargetRef {
                        path: related.clone(),
                        key: item.key,
                    }),
            );
        }
        out
    }

    fn types_in(
        &self,
        path: &str,
        name: &str,
        namespace: Option<&str>,
        rule: &'static str,
        span: Option<SourceSpan>,
    ) -> Vec<TypeHit> {
        let Some(file) = self.ctx.project.file(path) else {
            return Vec::new();
        };
        file.types
            .iter()
            .filter(|item| item.name == name && namespace.is_none_or(|ns| item.namespace == ns))
            .map(|item| TypeHit {
                path: path.to_owned(),
                ty: item.clone(),
                rule,
                span: span.clone(),
            })
            .collect()
    }

    /// Types visible by simple name from the call site.
    fn visible_types(&self, name: &str) -> VisibleTypes {
        let mut hits = Vec::new();
        let call_ns = self.call_namespace();
        for hit in self.types_in(self.ctx.path, name, None, "same-file-lexical", None) {
            if ns_visible(&call_ns, &hit.ty.namespace) {
                hits.push(hit);
            }
        }
        if !hits.is_empty() && !self.scheme.types_may_reopen() {
            return VisibleTypes {
                hits,
                external: None,
            };
        }
        let lookup = self.scheme.named_import(self.ctx, name);
        if hits.is_empty() {
            for import in &lookup.hits {
                if import.via_type.is_none() {
                    hits.extend(self.types_in(
                        &import.path,
                        &import.member,
                        import.namespace.as_deref(),
                        import.rule,
                        Some(import.span.clone()),
                    ));
                }
            }
            if !hits.is_empty() && !self.scheme.types_may_reopen() {
                return VisibleTypes {
                    hits,
                    external: None,
                };
            }
        }
        for peer in self.scheme.peers(self.ctx) {
            hits.extend(self.types_in(
                &peer.path,
                name,
                peer.namespace.as_deref(),
                peer.rule,
                peer.span.clone(),
            ));
        }
        dedupe_types(&mut hits);
        VisibleTypes {
            hits,
            external: lookup.external,
        }
    }

    /// Types reached through a qualifier path (`ns::Type`, `pkg.Type`, `Alias.Type`).
    fn qualified_types(&self, prefix: &[String], name: &str) -> VisibleTypes {
        let mut hits = Vec::new();
        let module = self.scheme.module_alias(self.ctx, prefix);
        for hit in &module.hits {
            hits.extend(self.types_in(
                &hit.path,
                name,
                hit.namespace.as_deref(),
                hit.rule,
                Some(hit.span.clone()),
            ));
        }
        if hits.is_empty() {
            for (path, file) in self.ctx.project.files() {
                if file.language != self.ctx.file.language {
                    continue;
                }
                for hit in self.types_in(path, name, None, "qualified-namespace", None) {
                    if ns_matches_path(&hit.ty.namespace, prefix) && !hit.ty.namespace.is_empty() {
                        hits.push(hit);
                    }
                }
            }
        }
        dedupe_types(&mut hits);
        VisibleTypes {
            hits,
            external: module.external,
        }
    }

    fn base_types(&self, hit: &TypeHit) -> Vec<TypeHit> {
        let mut found = Vec::new();
        for base in &hit.ty.bases {
            let simple = base.rsplit(['.', ':', '\\']).next().unwrap_or(base);
            let mut candidates = self.types_in(&hit.path, simple, None, "base-type", None);
            if candidates.is_empty() {
                candidates = self.visible_types(simple).hits;
            }
            if candidates.len() == 1 {
                found.extend(candidates);
            }
        }
        found
    }

    /// Declarations that may run for `name` on an instance of `hit`, following inheritance.
    fn dispatch(&self, hit: &TypeHit, name: &str, exact: bool) -> Vec<TargetRef> {
        let mut targets = self.methods(&hit.path, &hit.ty, name);
        let mut level = vec![hit.clone()];
        let mut depth = 0;
        while targets.is_empty() && depth < MAX_BASE_DEPTH {
            let mut next = Vec::new();
            for current in &level {
                next.extend(self.base_types(current));
            }
            dedupe_types(&mut next);
            if next.is_empty() {
                break;
            }
            for base in &next {
                targets.extend(self.methods(&base.path, &base.ty, name));
            }
            level = next;
            depth += 1;
        }
        if !exact {
            for sub in self.subtypes(hit, 0) {
                for target in self.methods(&sub.path, &sub.ty, name) {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
        }
        targets.sort();
        targets
    }

    fn subtypes(&self, hit: &TypeHit, depth: usize) -> Vec<TypeHit> {
        if depth >= MAX_BASE_DEPTH {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (path, file) in self.ctx.project.files() {
            if file.language != self.ctx.file.language {
                continue;
            }
            for ty in &file.types {
                let extends = ty
                    .bases
                    .iter()
                    .any(|base| base.rsplit(['.', ':', '\\']).next() == Some(hit.ty.name.as_str()));
                if extends && !(path == hit.path && ty.key == hit.ty.key) {
                    let sub = TypeHit {
                        path: path.to_owned(),
                        ty: ty.clone(),
                        rule: "subtype",
                        span: None,
                    };
                    found.push(sub.clone());
                    found.extend(self.subtypes(&sub, depth + 1));
                }
            }
        }
        found
    }

    // ----- outcomes -----

    fn targets_outcome(
        &self,
        mut targets: Vec<TargetRef>,
        rule: &'static str,
        what: &str,
        span: Option<SourceSpan>,
    ) -> Option<Outcome> {
        targets.sort();
        targets.dedup();
        match targets.len() {
            0 => None,
            1 => Some(Outcome::Resolved {
                target: targets.remove(0),
                rule,
                explanation: format!("resolved {what} to its single matching definition"),
                span,
            }),
            count => Some(Outcome::Possible {
                targets,
                rule,
                explanation: format!(
                    "{what} matches {count} definitions (overloads, redefinitions, or overriding \
                     subtypes); none is chosen"
                ),
                span,
            }),
        }
    }

    fn external_outcome(&self, external: Option<(String, SourceSpan)>) -> Option<Outcome> {
        external.map(|(module, span)| Outcome::External {
            name: module.clone(),
            rule: "external-import",
            explanation: format!(
                "`{}` is imported from `{module}`, which is outside the analysed workspace",
                self.call.name
            ),
            span: Some(span),
        })
    }

    // ----- bare calls -----

    fn function(&self) -> Outcome {
        let name = self.call.name.as_str();
        match self.lexical(name) {
            Lexical::Callable(key) => {
                return Outcome::Resolved {
                    target: TargetRef {
                        path: self.ctx.path.to_owned(),
                        key,
                    },
                    rule: "lexical-local-callable",
                    explanation: format!(
                        "`{name}` is bound to a callable declared in an enclosing scope"
                    ),
                    span: None,
                }
            }
            Lexical::Value => {
                return unresolved(
                    "local-value-callee",
                    format!(
                        "`{name}` is a local variable or parameter that shadows any definition; \
                         its value is not statically known"
                    ),
                )
            }
            Lexical::None => {}
        }
        if self.scheme.bare_call_sees_enclosing_type() {
            if let Some(container) = self.caller().and_then(|c| c.container.clone()) {
                let hits =
                    self.types_in(self.ctx.path, &container, None, "same-file-lexical", None);
                let mut targets = Vec::new();
                for hit in hits {
                    targets.extend(self.dispatch(&hit, name, false));
                }
                if let Some(outcome) = self.targets_outcome(
                    targets,
                    "implicit-self-method",
                    &format!("call to `{name}` inside `{container}`"),
                    None,
                ) {
                    return outcome;
                }
            }
        }
        let same_file = self.free_functions(self.ctx.path, name, None);
        if let Some(outcome) = self.targets_outcome(
            same_file,
            "same-file-lexical",
            &format!("call to `{name}`"),
            None,
        ) {
            return outcome;
        }
        let lookup = self.scheme.named_import(self.ctx, name);
        let mut targets = Vec::new();
        let mut rule = "static-import";
        let mut span = None;
        for hit in &lookup.hits {
            rule = hit.rule;
            span = Some(hit.span.clone());
            match &hit.via_type {
                Some(type_name) => {
                    for type_hit in self.types_in(&hit.path, type_name, None, hit.rule, None) {
                        targets.extend(self.methods(&hit.path, &type_hit.ty, &hit.member));
                    }
                }
                None => targets.extend(self.free_functions(
                    &hit.path,
                    &hit.member,
                    hit.namespace.as_deref(),
                )),
            }
        }
        if let Some(outcome) =
            self.targets_outcome(targets, rule, &format!("call to imported `{name}`"), span)
        {
            return outcome;
        }
        if !lookup.hits.is_empty() {
            // The import points into the workspace but no matching callable exists there; it may
            // still be a type being constructed.
            if let Some(outcome) = self.bare_type_call(name) {
                return outcome;
            }
            return unresolved(
                "imported-name-not-found",
                format!(
                    "the import of `{name}` resolves to a workspace file with no such callable"
                ),
            );
        }
        for peer in self
            .scheme
            .peers(self.ctx)
            .into_iter()
            .filter(|peer| peer.functions)
        {
            let targets = self.free_functions(&peer.path, name, peer.namespace.as_deref());
            if let Some(outcome) = self.targets_outcome(
                targets,
                peer.rule,
                &format!("call to `{name}`"),
                peer.span.clone(),
            ) {
                return outcome;
            }
        }
        if let Some(outcome) = self.bare_type_call(name) {
            return outcome;
        }
        if let Some(outcome) = self.external_outcome(lookup.external) {
            return outcome;
        }
        unresolved(
            "unresolved-name",
            format!("no statically visible definition of `{name}` was found"),
        )
    }

    // ----- construction -----

    fn bare_type_call(&self, name: &str) -> Option<Outcome> {
        match self.scheme.bare_type_call() {
            BareTypeCall::Nothing => None,
            BareTypeCall::Construct => {
                let visible = self.visible_types(name);
                self.construct_from(visible, name)
            }
            BareTypeCall::Conversion => {
                if self.visible_types(name).hits.is_empty() {
                    None
                } else {
                    Some(unresolved(
                        "type-conversion",
                        format!(
                            "`{name}(..)` converts a value to a workspace type; it is not a call"
                        ),
                    ))
                }
            }
        }
    }

    fn construct_from(&self, visible: VisibleTypes, name: &str) -> Option<Outcome> {
        if visible.hits.is_empty() {
            return visible.external.map(|(module, span)| Outcome::External {
                name: module.clone(),
                rule: "external-import",
                explanation: format!("type `{name}` comes from `{module}`, outside the workspace"),
                span: Some(span),
            });
        }
        if visible.hits.len() > 1 {
            let mut targets = Vec::new();
            for hit in &visible.hits {
                targets.extend(self.constructors(&hit.path, &hit.ty));
            }
            targets.sort();
            targets.dedup();
            if targets.is_empty() {
                return Some(unresolved(
                    "ambiguous-type",
                    format!("several workspace types are named `{name}`"),
                ));
            }
            return Some(Outcome::Possible {
                targets,
                rule: "ambiguous-type",
                explanation: format!("several workspace types are named `{name}`"),
                span: None,
            });
        }
        let hit = &visible.hits[0];
        let targets = self.constructors(&hit.path, &hit.ty);
        if targets.is_empty() {
            return Some(unresolved(
                "implicit-constructor",
                format!("`{name}` has no explicit constructor in the workspace"),
            ));
        }
        self.targets_outcome(
            targets,
            "constructor",
            &format!("construction of `{name}`"),
            hit.span.clone(),
        )
    }

    fn constructor(&self) -> Outcome {
        let name = self.call.name.as_str();
        let visible = match &self.call.receiver {
            Receiver::Path(prefix) if !prefix.is_empty() => self.qualified_types(prefix, name),
            _ => self.visible_types(name),
        };
        self.construct_from(visible, name).unwrap_or_else(|| {
            unresolved(
                "unresolved-type",
                format!("no statically visible type `{name}` was found"),
            )
        })
    }

    // ----- member calls -----

    fn member(&self) -> Outcome {
        let name = self.call.name.as_str();
        match &self.call.receiver {
            Receiver::SelfValue => self.self_member(name),
            Receiver::Super => self.super_member(name),
            Receiver::None | Receiver::Expr => unresolved(
                "dynamic-receiver",
                "the receiver is an expression whose type is not statically known",
            ),
            Receiver::Path(path) => self.path_member(path, name),
        }
    }

    fn enclosing_type(&self) -> Option<TypeHit> {
        let caller = self.caller()?;
        let container = caller.container.clone()?;
        self.types_in(
            self.ctx.path,
            &container,
            Some(caller.namespace.as_str()),
            "same-file-lexical",
            None,
        )
        .into_iter()
        .next()
        .or_else(|| {
            self.types_in(self.ctx.path, &container, None, "same-file-lexical", None)
                .into_iter()
                .next()
        })
    }

    fn self_member(&self, name: &str) -> Outcome {
        let Some(hit) = self.enclosing_type() else {
            return unresolved(
                "self-outside-type",
                "`self`/`this` is used where no enclosing type is known",
            );
        };
        let targets = self.dispatch(&hit, name, false);
        self.targets_outcome(
            targets,
            "self-method",
            &format!("`{name}` on `{}`", hit.ty.name),
            None,
        )
        .unwrap_or_else(|| {
            unresolved(
                "method-not-in-type",
                format!(
                    "`{name}` is not declared on `{}` or its workspace base types; mixins, \
                     dynamic members, or external bases are not modelled",
                    hit.ty.name
                ),
            )
        })
    }

    fn super_member(&self, name: &str) -> Outcome {
        let Some(hit) = self.enclosing_type() else {
            return unresolved(
                "self-outside-type",
                "`super` is used where no enclosing type is known",
            );
        };
        let mut targets = Vec::new();
        for base in self.base_types(&hit) {
            targets.extend(self.dispatch(&base, name, true));
        }
        self.targets_outcome(
            targets,
            "super-method",
            &format!("`super.{name}` from `{}`", hit.ty.name),
            None,
        )
        .unwrap_or_else(|| {
            unresolved(
                "super-not-found",
                "the base type or method is outside the workspace or not statically known",
            )
        })
    }

    fn path_member(&self, path: &[String], name: &str) -> Outcome {
        if path.len() == 1 {
            let root = path[0].as_str();
            if let Some(binding) = self.local_binding(root) {
                return self.typed_receiver(&binding, name);
            }
        }
        // `Alias.f()` / `pkg.sub.f()` where the path names a module or namespace.
        let module = self.scheme.module_alias(self.ctx, path);
        let mut targets = Vec::new();
        let mut rule = "module-member";
        let mut span = None;
        for hit in &module.hits {
            rule = hit.rule;
            span = Some(hit.span.clone());
            targets.extend(self.free_functions(&hit.path, name, hit.namespace.as_deref()));
        }
        if let Some(outcome) =
            self.targets_outcome(targets, rule, &format!("`{}.{name}`", path.join(".")), span)
        {
            return outcome;
        }
        // `Type.f()` / `ns::Type::f()` static or associated call.
        let owner = path.last().map(String::as_str).unwrap_or_default();
        let visible = if path.len() > 1 {
            let q = self.qualified_types(&path[..path.len() - 1], owner);
            if q.hits.is_empty() {
                self.visible_types(owner)
            } else {
                q
            }
        } else if matches!(owner, "Self" | "self") {
            VisibleTypes {
                hits: self.enclosing_type().into_iter().collect(),
                external: None,
            }
        } else {
            self.visible_types(owner)
        };
        let mut targets = Vec::new();
        let mut rule = "static-member";
        let mut span = None;
        for hit in &visible.hits {
            rule = if hit.rule == "same-file-lexical" {
                "static-member"
            } else {
                hit.rule
            };
            span = hit.span.clone();
            targets.extend(self.dispatch(hit, name, true));
        }
        if let Some(outcome) =
            self.targets_outcome(targets, rule, &format!("`{}.{name}`", path.join(".")), span)
        {
            return outcome;
        }
        // Free functions inside a namespace/inline module reached by path (`ns::f()`).
        if !path.is_empty() {
            let mut targets = Vec::new();
            for (candidate, file) in self.ctx.project.files() {
                if file.language != self.ctx.file.language {
                    continue;
                }
                for target in self.free_functions_in_namespace(candidate, name, path) {
                    targets.push(target);
                }
            }
            if let Some(outcome) = self.targets_outcome(
                targets,
                "qualified-namespace",
                &format!("`{}::{name}`", path.join("::")),
                None,
            ) {
                return outcome;
            }
        }
        if !visible.hits.is_empty() {
            return unresolved(
                "method-not-in-type",
                format!("`{name}` is not declared on `{owner}` in the workspace"),
            );
        }
        if let Some(outcome) = self.external_outcome(module.external.or(visible.external)) {
            return outcome;
        }
        unresolved(
            "dynamic-receiver",
            format!(
                "`{}` is not a known module, type, or typed local; the receiver is not statically known",
                path.join(".")
            ),
        )
    }

    fn free_functions_in_namespace(
        &self,
        path: &str,
        name: &str,
        prefix: &[String],
    ) -> Vec<TargetRef> {
        let Some(file) = self.ctx.project.file(path) else {
            return Vec::new();
        };
        let call_ns = self.call_namespace();
        file.callables
            .iter()
            .filter(|item| {
                item.parent.is_none()
                    && item.container.is_none()
                    && item.kind == CallableKind::Function
                    && item.name == name
                    && !item.namespace.is_empty()
                    && (ns_matches_path(&item.namespace, prefix)
                        || ns_matches_path(&format!("{call_ns}.{}", item.namespace), prefix))
            })
            .map(|item| TargetRef {
                path: path.to_owned(),
                key: item.key,
            })
            .collect()
    }

    fn typed_receiver(&self, binding: &LocalBinding, name: &str) -> Outcome {
        let Some(ty) = binding.ty.clone() else {
            return unresolved(
                "dynamic-receiver",
                format!(
                    "`{}` is a local with no statically known type",
                    binding.name
                ),
            );
        };
        let simple = ty.rsplit(['.', ':', '\\']).next().unwrap_or(&ty).to_owned();
        let visible = if ty.contains(['.', ':', '\\']) {
            let prefix = ty
                .split(['.', ':', '\\'])
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            self.qualified_types(&prefix[..prefix.len() - 1], &simple)
        } else {
            self.visible_types(&simple)
        };
        if visible.hits.is_empty() {
            if let Some(outcome) = self.external_outcome(visible.external) {
                return outcome;
            }
            return unresolved(
                "dynamic-receiver",
                format!("receiver type `{ty}` is not a workspace type"),
            );
        }
        let mut targets = Vec::new();
        for hit in &visible.hits {
            targets.extend(self.dispatch(hit, name, binding.ty_exact));
        }
        self.targets_outcome(
            targets,
            if binding.ty_exact {
                "typed-receiver"
            } else {
                "declared-type-receiver"
            },
            &format!("`{name}` on `{}`", simple),
            visible.hits[0].span.clone(),
        )
        .unwrap_or_else(|| {
            unresolved(
                "method-not-in-type",
                format!("`{name}` is not declared on `{simple}` or its workspace base types"),
            )
        })
    }
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
