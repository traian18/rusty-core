//! `Cargo.toml` adapter. Reads the file as data; Cargo is never invoked.
use std::path::Path;

use crate::repository_intelligence::{
    read_contained_file, DiscoveryDiagnostic, ManifestAdapter, ManifestInput, ManifestParse,
    Package, RootRelativePath, WorkspaceDeclaration,
};

const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
/// Tables whose dependencies do not ship in the compiled crate's API surface.
const DEV_TABLES: [&str; 2] = ["dev-dependencies", "build-dependencies"];
const TARGET_TABLES: [&str; 4] = ["bin", "test", "example", "bench"];

/// Supports packages, virtual workspaces, `[lib]`/target paths, and declared dependencies.
#[derive(Clone, Copy, Debug, Default)]
pub struct CargoManifestAdapter;

impl ManifestAdapter for CargoManifestAdapter {
    fn id(&self) -> &'static str {
        "cargo"
    }

    fn file_name(&self) -> &'static str {
        "Cargo.toml"
    }

    fn inherits_workspace_members(&self) -> bool {
        true
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let document = match toml::from_str::<toml::Value>(input.text) {
            Ok(document) => document,
            Err(error) => {
                return rejected(input, format!("malformed Cargo.toml: {error}"));
            }
        };
        let base = input.path.directory();
        if document
            .get("package")
            .and_then(toml::Value::as_table)
            .is_some()
        {
            parse_package(input, &document, base)
        } else if document
            .get("workspace")
            .and_then(toml::Value::as_table)
            .is_some()
        {
            ManifestParse {
                workspace: declared_workspace(&document, base),
                ..ManifestParse::default()
            }
        } else {
            rejected(
                input,
                "Cargo.toml has neither package nor workspace table".into(),
            )
        }
    }
}

fn rejected(input: &ManifestInput<'_>, message: String) -> ManifestParse {
    ManifestParse {
        diagnostics: vec![DiscoveryDiagnostic::for_path(input.path, message)],
        ..ManifestParse::default()
    }
}

fn parse_package(input: &ManifestInput<'_>, document: &toml::Value, base: &Path) -> ManifestParse {
    let name = document
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str);
    let Some(name) = name else {
        return rejected(input, "Cargo package has no valid name".into());
    };
    let (declared, dev) = declared_dependencies(document);
    let mut package = Package::declared(name.to_owned(), input.path.clone(), declared);
    package.facts.dev_dependencies = dev;
    package.facts.target_roots = explicit_target_roots(document, base);
    package.facts.source_roots = source_roots(input.root, document, base);
    let workspace = declared_workspace(document, base);
    if let Some(workspace) = &workspace {
        package.facts.workspace_members = workspace.members.clone();
    }
    ManifestParse {
        packages: vec![package],
        workspace,
        diagnostics: Vec::new(),
    }
}

/// All declared dependency names (including target-specific ones), and those declared only as
/// dev or build dependencies.
fn declared_dependencies(document: &toml::Value) -> (Vec<String>, Vec<String>) {
    let mut all = std::collections::BTreeSet::new();
    let mut regular = std::collections::BTreeSet::new();
    let mut collect_from = |container: &toml::Value| {
        for table in DEPENDENCY_TABLES {
            if let Some(values) = container.get(table).and_then(toml::Value::as_table) {
                all.extend(values.keys().cloned());
                if !DEV_TABLES.contains(&table) {
                    regular.extend(values.keys().cloned());
                }
            }
        }
    };
    collect_from(document);
    if let Some(targets) = document.get("target").and_then(toml::Value::as_table) {
        targets.values().for_each(collect_from);
    }
    let dev = all.difference(&regular).cloned().collect();
    (all.into_iter().collect(), dev)
}

/// Paths from `[[bin]]`, `[[test]]`, `[[example]]` and `[[bench]]` entries.
fn explicit_target_roots(document: &toml::Value, base: &Path) -> Vec<RootRelativePath> {
    let mut roots = TARGET_TABLES
        .iter()
        .filter_map(|table| document.get(*table).and_then(toml::Value::as_array))
        .flatten()
        .filter_map(|target| target.get("path").and_then(toml::Value::as_str))
        .filter_map(|path| RootRelativePath::new(base.join(path)).ok())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

/// The `[lib] path` (or `src/lib.rs`) followed by `src/main.rs`, keeping those that exist.
fn source_roots(root: &Path, document: &toml::Value, base: &Path) -> Vec<RootRelativePath> {
    let lib_path = document
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str);
    let mut roots = Vec::new();
    if let Some(lib_root) = lib_path.and_then(|path| RootRelativePath::new(base.join(path)).ok()) {
        roots.push(lib_root);
    }
    let defaults: &[&str] = if lib_path.is_some() {
        &["src/main.rs"]
    } else {
        &["src/lib.rs", "src/main.rs"]
    };
    for default in defaults {
        let candidate = base.join(default);
        if file_exists(root, &candidate) {
            roots.extend(RootRelativePath::new(candidate).ok());
        }
    }
    roots
}

fn file_exists(root: &Path, relative: &Path) -> bool {
    match read_contained_file(root, relative, 1) {
        Ok(_) => true,
        Err(error) => error.kind() == std::io::ErrorKind::FileTooLarge,
    }
}

fn declared_workspace(document: &toml::Value, base: &Path) -> Option<WorkspaceDeclaration> {
    let members = document
        .get("workspace")?
        .get("members")?
        .as_array()?
        .iter()
        .filter_map(toml::Value::as_str)
        .filter_map(|member| RootRelativePath::new(base.join(member)).ok())
        .collect();
    Some(WorkspaceDeclaration {
        base: base.to_path_buf(),
        members,
    })
}
