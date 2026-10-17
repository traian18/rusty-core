//! Static resolution of Rust `mod`, `use`, and `extern crate` against the Cargo module tree.
//!
//! * `module_tree` maps every file reachable from a Cargo target root to its module path.
//! * `crates` answers "what does this crate name mean" for a package.
//! * `use_paths` walks a path through modules and items, following `pub use` re-exports.
mod crates;
mod module_tree;
mod use_paths;

use harness_workspace::import_graph::{
    ImportBuildError, ImportResolver, LanguageProfile, ResolveContext, ResolveOutput,
    ResolvedImport,
};
use harness_workspace::repository_intelligence::DiscoveryDiagnostic;

use crates::CrateIndex;
use module_tree::ModuleTree;
use use_paths::PathResolver;

pub const RUST_RESOLVER_VERSION: u32 = 1;

const RULES: [&str; 13] = [
    "rust.mod_decl",
    "rust.mod_path_attribute",
    "rust.crate_root",
    "rust.self_module",
    "rust.super_module",
    "rust.relative_module",
    "rust.own_crate",
    "rust.workspace_sibling",
    "rust.declared_external",
    "rust.implicit_std",
    "rust.extern_crate",
    "rust.no_match",
    "rust.unattached",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct RustImportResolver;

impl ImportResolver for RustImportResolver {
    fn id(&self) -> &'static str {
        "rust-module-resolver"
    }

    fn version(&self) -> u32 {
        RUST_RESOLVER_VERSION
    }

    fn profiles(&self) -> Vec<LanguageProfile> {
        vec![LanguageProfile::Rust]
    }

    fn rules(&self) -> Vec<&'static str> {
        RULES.to_vec()
    }

    fn unsupported(&self) -> Vec<&'static str> {
        vec![
            "macro-generated modules and imports",
            "cfg evaluation: cfg-gated modules are treated as present",
            "computed or cfg_attr path attributes",
            "names bound by earlier use declarations",
            "renamed dependencies (package = ...)",
            "Rust 2015 path rules",
            "glob imports resolve to the module only",
        ]
    }

    fn resolve(&self, context: &ResolveContext<'_>) -> Result<ResolveOutput, ImportBuildError> {
        let units = context
            .units
            .iter()
            .filter(|unit| unit.profile == LanguageProfile::Rust)
            .collect::<Vec<_>>();
        let tree = ModuleTree::build(context, &units)?;
        let crates = CrateIndex::new(context.structure);
        let mut resolver = PathResolver::new(context, &units, &tree, &crates);
        let mut output = ResolveOutput::default();
        for unit in &units {
            if context.cancel.is_cancelled() {
                return Err(ImportBuildError::Cancelled);
            }
            if !tree.is_attached(&unit.path) {
                output.unattached.push(unit.path.clone());
                output.diagnostics.push(DiscoveryDiagnostic::for_path(
                    &unit.path,
                    "file is not reachable from any Cargo target root (unattached)",
                ));
            }
            for fact in &unit.extracted.facts {
                let (resolution, rule) = resolver.resolve_fact(unit, fact);
                if let Some(message) = resolver.take_limit_warning() {
                    output
                        .diagnostics
                        .push(DiscoveryDiagnostic::for_path(&unit.path, message));
                }
                output.resolved.push(ResolvedImport {
                    fact: fact.id.clone(),
                    resolution,
                    rule: rule.to_owned(),
                    evidence: Vec::new(),
                });
            }
        }
        Ok(output)
    }
}
