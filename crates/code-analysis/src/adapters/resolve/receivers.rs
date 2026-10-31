//! Resolving member calls by what the receiver is: `self`, `super`, a path or a typed local.

use super::*;

use super::super::facts::*;
use crate::model::CallableKind;

impl<S: Scheme> Engine<'_, S> {
    pub(super) fn enclosing_type(&self) -> Option<TypeHit> {
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

    pub(super) fn self_member(&self, name: &str) -> Outcome {
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

    pub(super) fn super_member(&self, name: &str) -> Outcome {
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

    pub(super) fn path_member(&self, path: &[String], name: &str) -> Outcome {
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

    pub(super) fn free_functions_in_namespace(
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

    pub(super) fn typed_receiver(&self, binding: &LocalBinding, name: &str) -> Outcome {
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
