//! `composer.json` read as text: package name, `require` and `require-dev`, and the PSR-4 map
//! (kept as inferred facts, `psr4:<Namespace\>=<directory>`). Composer is never run.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    DiscoveryDiagnostic, ManifestAdapter, ManifestInput, ManifestParse, Package,
};

pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![Arc::new(Composer)]
}

struct Composer;

fn requirements(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_object())
        .into_iter()
        .flatten()
        .map(|(name, _)| name.clone())
        .filter(|name| {
            name != "php"
                && !name.starts_with("ext-")
                && !name.starts_with("lib-")
                && name != "composer-plugin-api"
        })
        .collect()
}

impl ManifestAdapter for Composer {
    fn id(&self) -> &'static str {
        "composer-json"
    }

    fn file_name(&self) -> &'static str {
        "composer.json"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let value: serde_json::Value = match serde_json::from_str(input.text) {
            Ok(value) => value,
            Err(error) => {
                parse.diagnostics.push(DiscoveryDiagnostic::for_path(
                    input.path,
                    format!("composer.json is not valid JSON: {error}"),
                ));
                return parse;
            }
        };
        let name = value
            .get("name")
            .and_then(|n| n.as_str())
            .map(str::to_owned)
            .or_else(|| {
                input
                    .path
                    .directory()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "project".into());
        let runtime = requirements(value.get("require"));
        let dev = requirements(value.get("require-dev"));
        let mut all: Vec<String> = runtime.iter().chain(&dev).cloned().collect();
        all.sort();
        all.dedup();
        let mut package = Package::declared(name, input.path.clone(), all);
        package.facts.dev_dependencies = dev.into_iter().filter(|d| !runtime.contains(d)).collect();
        package.facts.dev_dependencies.sort();
        for section in ["autoload", "autoload-dev"] {
            let Some(map) = value
                .get(section)
                .and_then(|a| a.get("psr-4"))
                .and_then(|m| m.as_object())
            else {
                continue;
            };
            for (namespace, directories) in map {
                let list: Vec<&str> = match directories {
                    serde_json::Value::String(one) => vec![one.as_str()],
                    serde_json::Value::Array(many) => {
                        many.iter().filter_map(|d| d.as_str()).collect()
                    }
                    _ => Vec::new(),
                };
                for directory in list {
                    package
                        .facts
                        .inferred_facts
                        .push(format!("psr4:{namespace}={directory}"));
                }
            }
        }
        package.facts.inferred_facts.sort();
        parse.packages.push(package);
        parse
    }
}
