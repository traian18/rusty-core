//! `package.json` adapter for npm-style workspaces. No package manager is invoked.
use std::path::Path;

use serde_json::{Map, Value};

use crate::repository_intelligence::{
    DiscoveryDiagnostic, ManifestAdapter, ManifestInput, ManifestParse, Package, RootRelativePath,
    WorkspaceDeclaration,
};

const DEPENDENCY_FIELDS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];
const ENTRY_FIELDS: [&str; 4] = ["types", "typings", "module", "main"];
/// Order in which `exports` conditions are tried when picking a package entry.
const EXPORT_CONDITIONS: [&str; 4] = ["types", "import", "default", "require"];

/// Supports `workspaces` as an array or `{ "packages": [...] }`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PackageJsonAdapter;

impl ManifestAdapter for PackageJsonAdapter {
    fn id(&self) -> &'static str {
        "package-json"
    }

    fn file_name(&self) -> &'static str {
        "package.json"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let value = match serde_json::from_str::<Value>(input.text) {
            Ok(value) => value,
            Err(error) => return rejected(input, format!("malformed package.json: {error}")),
        };
        let Some(object) = value.as_object() else {
            return rejected(input, "package.json must contain an object".into());
        };
        let mut parse = ManifestParse::default();
        let base = input.path.directory();
        if object.contains_key("packageManager") {
            parse.diagnostics.push(DiscoveryDiagnostic::for_path(
                input.path,
                "packageManager declaration is unsupported",
            ));
        }
        parse.workspace = workspace_declaration(object, base);
        match object.get("name").and_then(Value::as_str) {
            Some(name) => parse
                .packages
                .push(package(input, object, name, &parse.workspace)),
            None if !object.contains_key("workspaces") => parse.diagnostics.push(
                DiscoveryDiagnostic::for_path(input.path, "package.json has no package name"),
            ),
            None => {}
        }
        parse
            .diagnostics
            .extend(workspace_form_diagnostic(input.path, object));
        parse
    }
}

fn rejected(input: &ManifestInput<'_>, message: String) -> ManifestParse {
    ManifestParse {
        diagnostics: vec![DiscoveryDiagnostic::for_path(input.path, message)],
        ..ManifestParse::default()
    }
}

fn package(
    input: &ManifestInput<'_>,
    object: &Map<String, Value>,
    name: &str,
    workspace: &Option<WorkspaceDeclaration>,
) -> Package {
    let (declared, dev) = declared_dependencies(object);
    let mut package = Package::declared(name.to_owned(), input.path.clone(), declared);
    package.facts.dev_dependencies = dev;
    package.facts.entry_points = entry_points(object, input.path.directory());
    if let Some(workspace) = workspace {
        package.facts.workspace_members = workspace.members.clone();
    }
    package
}

/// All declared dependency names, and those declared only as dev or peer dependencies.
fn declared_dependencies(object: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
    let names = |fields: &[&str]| {
        fields
            .iter()
            .filter_map(|field| object.get(*field).and_then(Value::as_object))
            .flat_map(|values| values.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let regular = names(&["dependencies", "optionalDependencies"]);
    let dev_like = names(&["devDependencies", "peerDependencies"]);
    let dev = dev_like.difference(&regular).cloned().collect();
    let all = DEPENDENCY_FIELDS
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_object))
        .flat_map(|values| values.keys().cloned())
        .collect();
    (all, dev)
}

/// Declared entry files in priority order; they are verified later against the file inventory.
fn entry_points(object: &Map<String, Value>, directory: &Path) -> Vec<RootRelativePath> {
    let from_fields = ENTRY_FIELDS
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str));
    let from_exports = object.get("exports").and_then(export_entry);
    from_fields
        .chain(from_exports)
        .filter_map(|entry| RootRelativePath::new(directory.join(entry)).ok())
        .collect()
}

/// Picks the package entry from `exports` using the fixed condition order.
fn export_entry(exports: &Value) -> Option<&str> {
    match exports {
        Value::String(path) => Some(path),
        Value::Array(items) => items.iter().find_map(export_entry),
        Value::Object(map) => match map.get(".") {
            Some(root) => export_entry(root),
            None => EXPORT_CONDITIONS
                .iter()
                .find_map(|condition| map.get(*condition).and_then(export_entry)),
        },
        _ => None,
    }
}

/// The workspace patterns when `workspaces` has a supported shape.
fn workspace_patterns(object: &Map<String, Value>) -> Option<&Vec<Value>> {
    let workspaces = object.get("workspaces")?;
    workspaces
        .as_array()
        .or_else(|| workspaces.get("packages").and_then(Value::as_array))
}

fn workspace_declaration(object: &Map<String, Value>, base: &Path) -> Option<WorkspaceDeclaration> {
    let members = workspace_patterns(object)?
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|pattern| RootRelativePath::new(base.join(pattern)).ok())
        .collect();
    Some(WorkspaceDeclaration {
        base: base.to_path_buf(),
        members,
    })
}

fn workspace_form_diagnostic(
    path: &RootRelativePath,
    object: &Map<String, Value>,
) -> Option<DiscoveryDiagnostic> {
    object.get("workspaces")?;
    let message = match workspace_patterns(object) {
        Some(patterns) if patterns.iter().all(Value::is_string) => return None,
        Some(_) => "workspace patterns must be strings",
        None => "unsupported package.json workspaces form",
    };
    Some(DiscoveryDiagnostic::for_path(path, message))
}
