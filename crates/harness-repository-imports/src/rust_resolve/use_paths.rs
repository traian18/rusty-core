//! Resolves `use`, `mod`, and `extern crate` facts to files and items.
use std::collections::HashMap;

use harness_workspace::{
    import_graph::{
        ImportFact, ImportForm, ImportReason, ItemStatus, ReexportHop, Resolution, ResolveContext,
        SourceUnit,
    },
    repository_intelligence::RootRelativePath,
};

use super::{
    crates::{CrateIndex, CrateLookup},
    module_tree::{ModTarget, Module, ModuleTree},
};

/// Upper bound on re-export lookups for one fact, guarding against cyclic or huge glob chains.
const STEP_BUDGET: usize = 2_000;

/// Where a path ended up, before it becomes a [`Resolution`].
struct Internal {
    file: RootRelativePath,
    item: Option<String>,
    status: ItemStatus,
    via: Vec<ReexportHop>,
    definition: Option<RootRelativePath>,
    /// The module a whole-module import names; needed to follow glob re-exports.
    module: Option<Module>,
}

enum Outcome {
    Internal(Internal),
    Other(Resolution),
}

type RuleOutcome = (Outcome, &'static str);

fn unresolved(reason: ImportReason, rule: &'static str) -> RuleOutcome {
    (Outcome::Other(Resolution::Unresolved { reason }), rule)
}

pub(super) struct PathResolver<'a> {
    context: &'a ResolveContext<'a>,
    units: HashMap<&'a RootRelativePath, &'a SourceUnit>,
    tree: &'a ModuleTree,
    crates: &'a CrateIndex<'a>,
    limit_warning: Option<String>,
}

impl<'a> PathResolver<'a> {
    pub fn new(
        context: &'a ResolveContext<'a>,
        units: &[&'a SourceUnit],
        tree: &'a ModuleTree,
        crates: &'a CrateIndex<'a>,
    ) -> Self {
        Self {
            context,
            units: units.iter().map(|unit| (&unit.path, *unit)).collect(),
            tree,
            crates,
            limit_warning: None,
        }
    }

    /// A message when the last resolution hit the re-export limit.
    pub fn take_limit_warning(&mut self) -> Option<String> {
        self.limit_warning.take()
    }

    pub fn resolve_fact(
        &mut self,
        unit: &SourceUnit,
        fact: &ImportFact,
    ) -> (Resolution, &'static str) {
        let mut steps = 0;
        match fact.form {
            ImportForm::Mod => self.resolve_mod(fact),
            ImportForm::ExternCrate => {
                let (outcome, _) =
                    self.resolve_crate_name(unit, &fact.spelling, &[], 0, &mut steps);
                (into_resolution(outcome), "rust.extern_crate")
            }
            _ => {
                let (outcome, rule) = self.resolve_use(unit, fact, 0, &mut steps);
                (into_resolution(outcome), rule)
            }
        }
    }

    fn resolve_mod(&self, fact: &ImportFact) -> (Resolution, &'static str) {
        match self.tree.mod_target(&fact.id) {
            None => (
                Resolution::Unresolved {
                    reason: ImportReason::UnattachedFile,
                },
                "rust.unattached",
            ),
            Some(ModTarget::Found(target, rule)) => (
                Resolution::ResolvedInternal {
                    target_file: target.clone(),
                    target_item: None,
                    item_status: ItemStatus::NotAttempted,
                    via: Vec::new(),
                    definition_file: None,
                },
                rule,
            ),
            Some(ModTarget::NotFound) => (
                Resolution::Unresolved {
                    reason: ImportReason::PathNotFound,
                },
                "rust.mod_decl",
            ),
            Some(ModTarget::Unsupported) => (
                Resolution::DynamicOrUnsupported {
                    reason: ImportReason::PathAttributeUnsupported,
                },
                "rust.mod_path_attribute",
            ),
        }
    }

    fn resolve_use(
        &mut self,
        unit: &SourceUnit,
        fact: &ImportFact,
        hops: usize,
        steps: &mut usize,
    ) -> RuleOutcome {
        let segments = fact.spelling.split("::").collect::<Vec<_>>();
        let current = self
            .tree
            .owner_of(&fact.importing_file)
            .map(|(root, path)| (root.clone(), [path.as_slice(), &fact.scope].concat()));
        match segments[0] {
            "Self" | "$crate" => (
                Outcome::Other(Resolution::DynamicOrUnsupported {
                    reason: ImportReason::UnsupportedForm,
                }),
                "rust.no_match",
            ),
            keyword @ ("crate" | "self" | "super") => match current {
                None => unresolved(ImportReason::UnattachedFile, "rust.unattached"),
                Some(current) => {
                    self.resolve_keyword_path(keyword, current, &segments, hops, steps)
                }
            },
            name => {
                if let Some(current) = current.as_ref().filter(|c| self.tree.has_child(c, name)) {
                    let outcome = self.walk(current.clone(), &segments, hops, steps);
                    return (outcome, "rust.relative_module");
                }
                self.resolve_crate_name(unit, name, &segments[1..], hops, steps)
            }
        }
    }

    /// Paths starting with `crate`, `self`, or `super`.
    fn resolve_keyword_path(
        &mut self,
        keyword: &str,
        (root, path): Module,
        segments: &[&str],
        hops: usize,
        steps: &mut usize,
    ) -> RuleOutcome {
        let (start, skip, rule) = match keyword {
            "crate" => ((root, Vec::new()), 1, "rust.crate_root"),
            "self" => ((root, path), 1, "rust.self_module"),
            _ => {
                let ups = segments.iter().take_while(|s| **s == "super").count();
                if ups > path.len() {
                    return unresolved(ImportReason::PathNotFound, "rust.super_module");
                }
                let kept = path.len() - ups;
                ((root, path[..kept].to_vec()), ups, "rust.super_module")
            }
        };
        (self.walk(start, &segments[skip..], hops, steps), rule)
    }

    /// Resolves a crate name used as the first segment of a path (or in `extern crate`).
    fn resolve_crate_name(
        &mut self,
        unit: &SourceUnit,
        name: &str,
        rest: &[&str],
        hops: usize,
        steps: &mut usize,
    ) -> RuleOutcome {
        let package = unit.package.as_ref().and_then(|id| self.crates.package(id));
        let (library, rule) = match self.crates.locate(package, name) {
            CrateLookup::Own(id) => (id, "rust.own_crate"),
            CrateLookup::Sibling(id) => (id, "rust.workspace_sibling"),
            CrateLookup::Ambiguous(ids) => return self.ambiguous_siblings(&ids),
            CrateLookup::External(declared) => {
                return (
                    Outcome::Other(Resolution::DeclaredExternal { package: declared }),
                    "rust.declared_external",
                )
            }
            CrateLookup::Std => {
                return (
                    Outcome::Other(Resolution::DeclaredExternal {
                        package: name.to_owned(),
                    }),
                    "rust.implicit_std",
                )
            }
            CrateLookup::Unknown => {
                return unresolved(ImportReason::UndeclaredExternal, "rust.no_match")
            }
        };
        match self.tree.lib_root(&library).cloned() {
            Some(lib) => (self.walk((lib, Vec::new()), rest, hops, steps), rule),
            None => unresolved(ImportReason::PathNotFound, "rust.no_match"),
        }
    }

    fn ambiguous_siblings(
        &self,
        ids: &[harness_workspace::repository_intelligence::PackageId],
    ) -> RuleOutcome {
        let limit = self.context.limits.max_candidates;
        let roots = ids
            .iter()
            .filter_map(|id| self.tree.lib_root(id).cloned())
            .collect::<Vec<_>>();
        (
            Outcome::Other(Resolution::Possible {
                truncated: roots.len() > limit,
                candidates: roots.into_iter().take(limit).collect(),
            }),
            "rust.workspace_sibling",
        )
    }

    /// Descends through child modules, then treats the next segment as an item.
    fn walk(
        &mut self,
        mut cursor: Module,
        rest: &[&str],
        hops: usize,
        steps: &mut usize,
    ) -> Outcome {
        let mut index = 0;
        while index < rest.len() && self.tree.has_child(&cursor, rest[index]) {
            cursor.1.push(rest[index].to_owned());
            index += 1;
        }
        let Some(file) = self.tree.file_of(&cursor).cloned() else {
            return Outcome::Other(Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            });
        };
        let Some(item) = rest.get(index) else {
            return Outcome::Internal(Internal {
                file,
                item: None,
                status: ItemStatus::NotAttempted,
                via: Vec::new(),
                definition: None,
                module: Some(cursor),
            });
        };
        let scope = self.tree.inline_scope(&file, &cursor.1);
        match self.find_item(&file, &scope, item, hops, steps) {
            Some((via, definition)) => Outcome::Internal(Internal {
                file,
                item: Some((*item).to_owned()),
                status: ItemStatus::Found,
                via,
                definition,
                module: None,
            }),
            // A missing name followed by more segments is a broken path, not a missing leaf item.
            None if rest.len() > index + 1 => Outcome::Other(Resolution::Unresolved {
                reason: ImportReason::PathNotFound,
            }),
            None => Outcome::Internal(Internal {
                file,
                item: Some((*item).to_owned()),
                status: ItemStatus::NotFound,
                via: Vec::new(),
                definition: None,
                module: None,
            }),
        }
    }

    /// Looks for `item` declared in `file`/`scope`, following `pub use` chains.
    /// Returns the re-export hops and the defining file when it is not `file`.
    fn find_item(
        &mut self,
        file: &RootRelativePath,
        scope: &[String],
        item: &str,
        hops: usize,
        steps: &mut usize,
    ) -> Option<(Vec<ReexportHop>, Option<RootRelativePath>)> {
        let unit = self.units.get(file).copied()?;
        let declared = unit
            .extracted
            .items
            .iter()
            .any(|declared| declared.name == item && declared.scope == scope);
        if declared {
            return Some((Vec::new(), None));
        }
        if hops >= self.context.limits.max_reexport_hops || *steps >= STEP_BUDGET {
            self.limit_warning = Some(format!(
                "re-export chain exceeded the configured limit while resolving `{item}`"
            ));
            return None;
        }
        *steps += 1;
        for fact in reexports_of(unit, scope, item) {
            let hop = ReexportHop {
                file: file.clone(),
                fact: fact.id.clone(),
            };
            let (outcome, _) = self.resolve_use(unit, fact, hops + 1, steps);
            let Outcome::Internal(found) = outcome else {
                continue;
            };
            if let Some((mut via, definition)) =
                self.follow_reexport(fact, found, item, hops, steps)
            {
                via.insert(0, hop);
                return Some((via, definition));
            }
        }
        None
    }

    /// Continues a lookup through one `pub use`: into the glob's module, or past a named re-export.
    fn follow_reexport(
        &mut self,
        fact: &ImportFact,
        target: Internal,
        item: &str,
        hops: usize,
        steps: &mut usize,
    ) -> Option<(Vec<ReexportHop>, Option<RootRelativePath>)> {
        if fact.glob {
            let scope = target
                .module
                .as_ref()
                .map(|(_, path)| self.tree.inline_scope(&target.file, path))
                .unwrap_or_default();
            let (via, definition) = self.find_item(&target.file, &scope, item, hops + 1, steps)?;
            return Some((via, definition.or(Some(target.file))));
        }
        if target.status == ItemStatus::NotFound {
            return None;
        }
        Some((target.via, target.definition.or(Some(target.file))))
    }
}

/// `pub use` declarations in `scope` that could provide `item`.
fn reexports_of<'u>(unit: &'u SourceUnit, scope: &[String], item: &str) -> Vec<&'u ImportFact> {
    unit.extracted
        .facts
        .iter()
        .filter(|fact| {
            fact.form == ImportForm::Use
                && fact.reexport
                && fact.scope == scope
                && (fact.glob
                    || fact.alias.as_deref().or(fact.imported_name.as_deref()) == Some(item))
        })
        .collect()
}

fn into_resolution(outcome: Outcome) -> Resolution {
    match outcome {
        Outcome::Other(resolution) => resolution,
        Outcome::Internal(found) => Resolution::ResolvedInternal {
            target_file: found.file,
            target_item: found.item,
            item_status: found.status,
            via: found.via,
            definition_file: found.definition,
        },
    }
}
