//! Finding candidates: functions, methods, constructors and types by name, with the namespace and inheritance rules of the language.

use super::*;

use super::super::facts::*;
use crate::model::{CallableKind, SourceSpan};

impl<S: Scheme> Engine<'_, S> {
    pub(super) fn caller(&self) -> Option<&CallableFact> {
        self.ctx.file.callable(self.call.caller)
    }

    pub(super) fn call_namespace(&self) -> String {
        self.ctx.namespace.to_owned()
    }

    pub(super) fn lexical(&self, name: &str) -> Lexical {
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

    pub(super) fn local_binding(&self, name: &str) -> Option<LocalBinding> {
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

    pub(super) fn free_functions(
        &self,
        path: &str,
        name: &str,
        namespace: Option<&str>,
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

    pub(super) fn methods(&self, path: &str, ty: &TypeFact, name: &str) -> Vec<TargetRef> {
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

    pub(super) fn constructors(&self, path: &str, ty: &TypeFact) -> Vec<TargetRef> {
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

    pub(super) fn types_in(
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
    pub(super) fn visible_types(&self, name: &str) -> VisibleTypes {
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
    pub(super) fn qualified_types(&self, prefix: &[String], name: &str) -> VisibleTypes {
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

    pub(super) fn base_types(&self, hit: &TypeHit) -> Vec<TypeHit> {
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
    pub(super) fn dispatch(&self, hit: &TypeHit, name: &str, exact: bool) -> Vec<TargetRef> {
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

    pub(super) fn subtypes(&self, hit: &TypeHit, depth: usize) -> Vec<TypeHit> {
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

    pub(super) fn targets_outcome(
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

    pub(super) fn external_outcome(
        &self,
        external: Option<(String, SourceSpan)>,
    ) -> Option<Outcome> {
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
}
