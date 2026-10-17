//! The module tree of each Cargo target: which file defines which module path.
//!
//! A file reachable from several roots belongs to the first root in priority order
//! (library, binary, explicit targets, then Cargo's conventional layout).
use std::collections::{BTreeSet, HashMap};

use harness_workspace::{
    import_graph::{
        ImportBuildError, ImportFact, ImportFactId, ImportForm, ItemKind, PathAttribute,
        ResolveContext, SourceUnit,
    },
    repository_intelligence::{Package, PackageId, RootRelativePath},
};

use crate::paths::{file_name, join_normalized, parent};

/// A module: the target root it belongs to and its path from that root.
pub(super) type Module = (RootRelativePath, Vec<String>);

/// Where a `mod foo;` declaration points.
#[derive(Clone)]
pub(super) enum ModTarget {
    Found(RootRelativePath, &'static str),
    NotFound,
    Unsupported,
}

pub(super) struct ModuleTree {
    /// File → (target root, module path of the file itself).
    owner: HashMap<RootRelativePath, Module>,
    /// Module → file defining it (inline modules share their parent's file).
    modules: HashMap<Module, RootRelativePath>,
    mod_targets: HashMap<ImportFactId, ModTarget>,
    lib_roots: HashMap<PackageId, RootRelativePath>,
}

impl ModuleTree {
    pub fn build(
        context: &ResolveContext<'_>,
        units: &[&SourceUnit],
    ) -> Result<Self, ImportBuildError> {
        let mut builder = Builder {
            units: units.iter().map(|unit| (&unit.path, *unit)).collect(),
            tree: ModuleTree {
                owner: HashMap::new(),
                modules: HashMap::new(),
                mod_targets: HashMap::new(),
                lib_roots: HashMap::new(),
            },
        };
        for package in context
            .structure
            .packages
            .iter()
            .filter(|p| file_name(&p.manifest) == "Cargo.toml")
        {
            builder.record_lib_root(package);
            for root in builder.roots_of(package) {
                if context.cancel.is_cancelled() {
                    return Err(ImportBuildError::Cancelled);
                }
                if !builder.tree.owner.contains_key(&root) {
                    builder.attach(&root);
                }
            }
        }
        Ok(builder.tree)
    }

    pub fn is_attached(&self, file: &RootRelativePath) -> bool {
        self.owner.contains_key(file)
    }

    /// The (root, module path) a file is the module of.
    pub fn owner_of(&self, file: &RootRelativePath) -> Option<&Module> {
        self.owner.get(file)
    }

    pub fn file_of(&self, module: &Module) -> Option<&RootRelativePath> {
        self.modules.get(module)
    }

    pub fn has_child(&self, parent: &Module, name: &str) -> bool {
        let mut path = parent.1.clone();
        path.push(name.to_owned());
        self.modules.contains_key(&(parent.0.clone(), path))
    }

    pub fn mod_target(&self, fact: &ImportFactId) -> Option<&ModTarget> {
        self.mod_targets.get(fact)
    }

    pub fn lib_root(&self, package: &PackageId) -> Option<&RootRelativePath> {
        self.lib_roots.get(package)
    }

    /// Inline-module nesting of `module_path` relative to the file that holds it.
    pub fn inline_scope(&self, file: &RootRelativePath, module_path: &[String]) -> Vec<String> {
        match self.owner.get(file) {
            Some((_, file_path)) if module_path.starts_with(file_path) => {
                module_path[file_path.len()..].to_vec()
            }
            _ => Vec::new(),
        }
    }
}

struct Builder<'a> {
    units: HashMap<&'a RootRelativePath, &'a SourceUnit>,
    tree: ModuleTree,
}

impl Builder<'_> {
    /// The library root is the first declared root that is not a binary entry point.
    fn record_lib_root(&mut self, package: &Package) {
        let lib = package
            .facts
            .source_roots
            .iter()
            .find(|root| file_name(root) != "main.rs")
            .filter(|root| self.units.contains_key(*root));
        if let Some(lib) = lib {
            self.tree.lib_roots.insert(package.id.clone(), lib.clone());
        }
    }

    /// Target roots in priority order: lib, main, explicit targets, then Cargo's conventional layout.
    fn roots_of(&self, package: &Package) -> Vec<RootRelativePath> {
        let mut roots = package.facts.source_roots.clone();
        roots.extend(package.facts.target_roots.iter().cloned());
        roots.extend(self.conventional_roots(package));
        roots.retain(|root| self.units.contains_key(root));
        roots
    }

    /// `build.rs`, `src/bin/*`, `tests/*`, `examples/*` and `benches/*` entry files.
    fn conventional_roots(&self, package: &Package) -> BTreeSet<RootRelativePath> {
        let base = parent(&package.manifest);
        self.units
            .iter()
            .filter(|(_, unit)| unit.package.as_ref() == Some(&package.id))
            .filter_map(|(path, _)| {
                let relative = path.as_path().strip_prefix(base).ok()?;
                let parts = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                is_conventional_root(&parts).then(|| (*path).clone())
            })
            .collect()
    }

    /// Walks `mod` declarations from `root`, recording ownership and module paths.
    fn attach(&mut self, root: &RootRelativePath) {
        let mut pending = vec![(root.clone(), Vec::<String>::new())];
        while let Some((file, path)) = pending.pop() {
            self.tree
                .owner
                .insert(file.clone(), (root.clone(), path.clone()));
            self.tree
                .modules
                .insert((root.clone(), path.clone()), file.clone());
            let Some(unit) = self.units.get(&file).copied() else {
                continue;
            };
            let children = self.register_modules_of(unit, root, &path);
            pending.extend(children.into_iter().rev());
        }
    }

    /// Registers each module declared in `unit`; returns external module files still to visit.
    fn register_modules_of(
        &mut self,
        unit: &SourceUnit,
        root: &RootRelativePath,
        path: &[String],
    ) -> Vec<(RootRelativePath, Vec<String>)> {
        let mut children = Vec::new();
        let module_items = unit
            .extracted
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Module);
        for item in module_items {
            let module_path = [path, &item.scope, std::slice::from_ref(&item.name)].concat();
            let declaration = unit.extracted.facts.iter().find(|fact| {
                fact.form == ImportForm::Mod
                    && fact.scope == item.scope
                    && fact.spelling == item.name
            });
            let Some(fact) = declaration else {
                self.tree
                    .modules
                    .insert((root.clone(), module_path), unit.path.clone());
                continue;
            };
            let target = self.locate_module_file(&unit.path, root, fact);
            if let ModTarget::Found(child, _) = &target {
                if !self.tree.owner.contains_key(child) {
                    children.push((child.clone(), module_path));
                }
            }
            self.tree.mod_targets.insert(fact.id.clone(), target);
        }
        children
    }

    /// Applies Rust's file layout rules (`foo.rs`, `foo/mod.rs`, `#[path]`) to a `mod foo;`.
    fn locate_module_file(
        &self,
        file: &RootRelativePath,
        root: &RootRelativePath,
        fact: &ImportFact,
    ) -> ModTarget {
        let inline_dir = fact
            .scope
            .iter()
            .fold(self.directory_for_children(file, root), |dir, segment| {
                dir.join(segment)
            });
        match &fact.path_attribute {
            PathAttribute::Unsupported => ModTarget::Unsupported,
            PathAttribute::Literal(literal) => {
                let base = if fact.scope.is_empty() {
                    parent(file).to_path_buf()
                } else {
                    inline_dir
                };
                join_normalized(&base, literal)
                    .filter(|path| self.units.contains_key(path))
                    .map_or(ModTarget::NotFound, |target| {
                        ModTarget::Found(target, "rust.mod_path_attribute")
                    })
            }
            PathAttribute::None => [
                inline_dir.join(format!("{}.rs", fact.spelling)),
                inline_dir.join(&fact.spelling).join("mod.rs"),
            ]
            .into_iter()
            .filter_map(|candidate| RootRelativePath::new(candidate).ok())
            .find(|candidate| self.units.contains_key(candidate))
            .map_or(ModTarget::NotFound, |target| {
                ModTarget::Found(target, "rust.mod_decl")
            }),
        }
    }

    /// Crate roots and `mod.rs` hold their children beside them; other files use a folder named after the file.
    fn directory_for_children(
        &self,
        file: &RootRelativePath,
        root: &RootRelativePath,
    ) -> std::path::PathBuf {
        let mut directory = parent(file).to_path_buf();
        if file != root && file_name(file) != "mod.rs" {
            if let Some(stem) = file.as_path().file_stem() {
                directory.push(stem);
            }
        }
        directory
    }
}

fn is_conventional_root(parts: &[String]) -> bool {
    let is_target_dir = |dir: &String| matches!(dir.as_str(), "tests" | "examples" | "benches");
    match parts {
        [build] => build == "build.rs",
        [dir, file] if is_target_dir(dir) => file.ends_with(".rs"),
        [src, bin, file] if src == "src" && bin == "bin" => file.ends_with(".rs"),
        [dir, _, main] if is_target_dir(dir) => main == "main.rs",
        [src, bin, _, main] if src == "src" && bin == "bin" => main == "main.rs",
        _ => false,
    }
}
