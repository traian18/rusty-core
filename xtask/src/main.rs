#![warn(clippy::all)]

use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    path::Path,
    process::{Command, ExitCode},
};

use serde_json::Value;

const PROTECTED_PACKAGES: [&str; 2] = ["harness-core", "harness-protocol"];

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("check-deps") => match check_dependencies() {
            Ok(()) => {
                println!("dependency direction checks passed");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("dependency direction check failed: {error}");
                ExitCode::FAILURE
            }
        },
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            print_usage();
            ExitCode::FAILURE
        }
        None => {
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo run --manifest-path xtask/Cargo.toml -- check-deps");
}

fn check_dependencies() -> Result<(), String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .ok_or_else(|| "xtask manifest has no parent directory".to_owned())?;

    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--all-features"])
        .current_dir(workspace_root)
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "cargo metadata exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    validate_graph(&metadata)
}

fn validate_graph(metadata: &Value) -> Result<(), String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not contain packages".to_owned())?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not contain a resolved dependency graph".to_owned())?;

    let mut names = HashMap::new();
    for package in packages {
        let id = required_string(package, "id")?;
        let name = required_string(package, "name")?;
        names.insert(id.to_owned(), name.to_owned());
    }

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut features: HashMap<String, HashSet<String>> = HashMap::new();
    for node in nodes {
        let id = required_string(node, "id")?.to_owned();
        let dependencies = node["deps"]
            .as_array()
            .ok_or_else(|| format!("metadata node {id} did not contain deps"))?
            .iter()
            .map(|dependency| required_string(dependency, "pkg").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let enabled_features = node["features"]
            .as_array()
            .ok_or_else(|| format!("metadata node {id} did not contain features"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        graph.insert(id.clone(), dependencies);
        features.insert(id, enabled_features);
    }

    for protected_name in PROTECTED_PACKAGES {
        let root_id = names
            .iter()
            .find_map(|(id, name)| (name == protected_name).then_some(id))
            .ok_or_else(|| format!("workspace package {protected_name} was not found"))?;
        validate_reachable(root_id, &names, &graph, &features)?;
    }

    Ok(())
}

fn validate_reachable(
    root_id: &str,
    names: &HashMap<String, String>,
    graph: &HashMap<String, Vec<String>>,
    features: &HashMap<String, HashSet<String>>,
) -> Result<(), String> {
    let mut queue = VecDeque::from([root_id.to_owned()]);
    let mut visited = HashSet::from([root_id.to_owned()]);
    let mut parent: HashMap<String, String> = HashMap::new();

    while let Some(package_id) = queue.pop_front() {
        let package_name = names
            .get(&package_id)
            .ok_or_else(|| format!("resolved package {package_id} has no package entry"))?;

        if package_id != root_id {
            let forbidden_reason = forbidden_reason(
                package_name,
                features.get(&package_id).cloned().unwrap_or_default(),
            );
            if let Some(reason) = forbidden_reason {
                let path = dependency_path(root_id, &package_id, &parent, names);
                let predecessor_id = parent
                    .get(&package_id)
                    .ok_or_else(|| format!("missing predecessor for {package_name}"))?;
                let predecessor = names
                    .get(predecessor_id)
                    .map(String::as_str)
                    .unwrap_or(predecessor_id);
                return Err(format!(
                    "{} -> {} violates core isolation ({reason}); dependency path: {}",
                    predecessor,
                    package_name,
                    path.join(" -> ")
                ));
            }
        }

        if let Some(dependencies) = graph.get(&package_id) {
            for dependency_id in dependencies {
                if visited.insert(dependency_id.clone()) {
                    parent.insert(dependency_id.clone(), package_id.clone());
                    queue.push_back(dependency_id.clone());
                }
            }
        }
    }

    Ok(())
}

fn forbidden_reason(name: &str, features: HashSet<String>) -> Option<&'static str> {
    if name == "tokio"
        && features.iter().any(|feature| {
            matches!(
                feature.as_str(),
                "net" | "process" | "signal" | "fs" | "io-std"
            )
        })
    {
        return Some("Tokio I/O, process, or networking features are forbidden");
    }

    if name.starts_with("harness-integration-") {
        return Some("provider integrations must remain outside core");
    }
    if name.starts_with("harness-tool-") {
        return Some("concrete tool implementations must remain outside core");
    }
    if name.starts_with("harness-transport-") {
        return Some("transport implementations must remain outside core");
    }

    // Not covered by any of the prefix rules above, and it walks the
    // filesystem — so it needs naming explicitly, or a `harness-core ->
    // harness-skills` edge would slip through the guard.
    if name == "harness-skills" {
        return Some("skill discovery performs filesystem I/O and must remain outside core");
    }

    match name {
        "reqwest" | "hyper" | "rusqlite" | "sqlx" | "tauri" | "ratatui" | "tokio-tungstenite"
        | "tungstenite" | "jsonrpsee" | "rmcp" => {
            Some("I/O, UI, transport, provider, or persistence dependency is forbidden")
        }
        _ => None,
    }
}

fn dependency_path(
    root_id: &str,
    target_id: &str,
    parent: &HashMap<String, String>,
    names: &HashMap<String, String>,
) -> Vec<String> {
    let mut ids = vec![target_id.to_owned()];
    let mut current = target_id;
    while current != root_id {
        let Some(previous) = parent.get(current) else {
            break;
        };
        ids.push(previous.clone());
        current = previous;
    }
    ids.reverse();
    ids.into_iter()
        .map(|id| names.get(&id).cloned().unwrap_or(id))
        .collect()
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("cargo metadata field {field} was missing or not a string"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_graph;

    #[test]
    fn reports_the_violating_edge_and_transitive_path() {
        let metadata = json!({
            "packages": [
                {"id": "core", "name": "harness-core"},
                {"id": "protocol", "name": "harness-protocol"},
                {"id": "helper", "name": "pure-helper"},
                {"id": "provider", "name": "harness-integration-anthropic"}
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "core",
                        "deps": [{"pkg": "protocol"}, {"pkg": "helper"}],
                        "features": []
                    },
                    {"id": "protocol", "deps": [], "features": []},
                    {
                        "id": "helper",
                        "deps": [{"pkg": "provider"}],
                        "features": []
                    },
                    {"id": "provider", "deps": [], "features": []}
                ]
            }
        });

        let error = validate_graph(&metadata).expect_err("the forbidden edge must fail");
        assert!(error.contains("pure-helper -> harness-integration-anthropic"));
        assert!(error.contains("harness-core -> pure-helper -> harness-integration-anthropic"));
    }
}
