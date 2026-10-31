//! Resolving the shapes of call: a bare function, a type used as a constructor, a member of a receiver.

use super::*;

use super::super::facts::*;

impl<S: Scheme> Engine<'_, S> {
    pub(super) fn function(&self) -> Outcome {
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

    pub(super) fn bare_type_call(&self, name: &str) -> Option<Outcome> {
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

    pub(super) fn construct_from(&self, visible: VisibleTypes, name: &str) -> Option<Outcome> {
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

    pub(super) fn constructor(&self) -> Outcome {
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

    pub(super) fn member(&self) -> Outcome {
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
}
