//! The workspace's own packages: where each lives and which other workspace packages it uses.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

/// One member of the workspace.
pub struct Package {
    pub name: String,
    /// The folder holding its `Cargo.toml`.
    pub folder: PathBuf,
    /// Workspace packages it depends on in normal or build code (not dev-only).
    pub dependencies: BTreeSet<String>,
}

impl Package {
    /// `crates/rusty-agent-core`, `apps/harnessd`: the folder relative to the workspace root.
    pub fn relative_folder(&self, root: &Path) -> String {
        let relative = self.folder.strip_prefix(root).unwrap_or(&self.folder);
        relative.to_string_lossy().into_owned()
    }
}

/// The folder holding the workspace `Cargo.toml`.
pub fn workspace_root() -> Result<PathBuf, String> {
    let xtask_folder = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = xtask_folder
        .parent()
        .ok_or_else(|| "xtask manifest has no parent directory".to_owned())?;
    Ok(root.to_path_buf())
}

pub fn load_packages(root: &Path) -> Result<Vec<Package>, String> {
    let metadata = read_metadata(root)?;
    let members = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not contain packages".to_owned())?;
    let names: BTreeSet<&str> = members.iter().filter_map(|p| p["name"].as_str()).collect();
    members
        .iter()
        .map(|member| read_package(member, &names))
        .collect()
}

fn read_metadata(root: &Path) -> Result<Value, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cargo metadata exited with {}: {}",
            output.status,
            reason.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))
}

fn read_package(member: &Value, workspace_names: &BTreeSet<&str>) -> Result<Package, String> {
    let name = text_field(member, "name")?;
    let manifest = text_field(member, "manifest_path")?;
    let folder = Path::new(manifest)
        .parent()
        .ok_or_else(|| format!("manifest of {name} has no parent folder"))?;
    Ok(Package {
        name: name.to_owned(),
        folder: folder.to_path_buf(),
        dependencies: workspace_dependencies(member, workspace_names),
    })
}

fn workspace_dependencies(member: &Value, workspace_names: &BTreeSet<&str>) -> BTreeSet<String> {
    let dependencies = member["dependencies"].as_array().into_iter().flatten();
    dependencies
        .filter(|dependency| dependency["kind"] != "dev")
        .filter_map(|dependency| dependency["name"].as_str())
        .filter(|name| workspace_names.contains(name))
        .map(str::to_owned)
        .collect()
}

fn text_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("cargo metadata field {field} was missing or not a string"))
}
