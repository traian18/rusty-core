//! `check-layers`: the arrows between the layers of the system point one way.
//!
//! Engine crates run agents; intelligence crates read repositories. Neither imports the other.
//! Only bridge crates may see both, and only assembly crates (the engine builder, the apps) may
//! see everything. Product crates build on intelligence. Every package must be classified here,
//! so a new crate cannot appear without someone deciding which layer it belongs to.

use std::collections::BTreeMap;

use crate::{report::Finding, workspace::Package};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    /// Protocol, state machine, runtime, providers, tools, transports, stores.
    Engine,
    /// Reads repositories: imports, analysis, health, findings, wiki.
    Intelligence,
    /// Features built on the layers below: specs, goals, guidelines.
    Product,
    /// The only crates that connect engine and intelligence.
    Bridge,
    /// Builds a working system from everything: the engine builder, binaries, the SDK.
    Assembly,
}

/// Intelligence crates the engine may use directly: the shared file-system abstraction.
const SHARED_WITH_ENGINE: [&str; 1] = ["rusty-workspace"];

pub fn check_layers(packages: &[Package]) -> Vec<Finding> {
    let layers: BTreeMap<&str, Option<Layer>> = packages
        .iter()
        .map(|package| (package.name.as_str(), layer_of(&package.name)))
        .collect();
    let mut findings = Vec::new();
    for package in packages {
        let Some(own) = layers[package.name.as_str()] else {
            findings.push(Finding::new(
                &package.name,
                "is not assigned to a layer in xtask/src/check_layers.rs",
            ));
            continue;
        };
        findings.extend(forbidden_edges(package, own, &layers));
    }
    findings
}

fn forbidden_edges(
    package: &Package,
    own: Layer,
    layers: &BTreeMap<&str, Option<Layer>>,
) -> Vec<Finding> {
    package
        .dependencies
        .iter()
        .filter_map(|dependency| {
            let target = layers.get(dependency.as_str()).copied().flatten()?;
            let allowed = may_depend_on(own, target, dependency);
            (!allowed).then(|| {
                Finding::new(
                    &package.name,
                    format!("{own:?} crate depends on {target:?} crate {dependency}"),
                )
            })
        })
        .collect()
}

fn may_depend_on(from: Layer, to: Layer, target_name: &str) -> bool {
    use Layer::{Assembly, Bridge, Engine, Intelligence, Product};
    match (from, to) {
        (Assembly | Bridge, _) => true,
        (_, Assembly) => false,
        (Engine, Engine) | (Intelligence, Intelligence) | (Product, Product) => true,
        (Product, Intelligence) | (Product, Engine) => true,
        (Engine, Intelligence) => SHARED_WITH_ENGINE.contains(&target_name),
        _ => false,
    }
}

/// `None` for a package nobody has classified yet.
fn layer_of(name: &str) -> Option<Layer> {
    if let Some(layer) = named_layer(name) {
        return Some(layer);
    }
    const ENGINE_PREFIXES: [&str; 5] = [
        "rusty-provider-",
        "rusty-transport-",
        "rusty-tool-",
        "rusty-protocol",
        "rusty-agent-core",
    ];
    const INTELLIGENCE_PREFIXES: [&str; 1] = ["rusty-lang-"];
    let starts_with = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
    if starts_with(&ENGINE_PREFIXES) {
        Some(Layer::Engine)
    } else if starts_with(&INTELLIGENCE_PREFIXES) {
        Some(Layer::Intelligence)
    } else {
        None
    }
}

fn named_layer(name: &str) -> Option<Layer> {
    let layer = match name {
        "rusty-workflow-core"
        | "rusty-workflow-runtime"
        | "rusty-runtime"
        | "rusty-tool-api"
        | "rusty-model-backend"
        | "rusty-context"
        | "rusty-session-store"
        | "rusty-skills"
        | "rusty-extension-api" => Layer::Engine,
        "rusty-workspace"
        | "rusty-repo-model"
        | "rusty-frameworks"
        | "rusty-code-analysis"
        | "rusty-git-analysis"
        | "rusty-findings"
        | "rusty-code-health"
        | "rusty-infra-analysis"
        | "rusty-repo-wiki" => Layer::Intelligence,
        "rusty-specs" | "rusty-goals" | "rusty-guidelines" => Layer::Product,
        "rusty-context-repo" | "rusty-tool-repo" | "rusty-tool-code" => Layer::Bridge,
        "rusty-engine" | "harness" | "harnessd" | "harnessctl" | "rusty-harness-sdk" => {
            Layer::Assembly
        }
        _ => return None,
    };
    Some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, dependencies: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            folder: "/nowhere".into(),
            dependencies: dependencies.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    #[test]
    fn the_engine_may_use_the_shared_workspace_but_not_other_intelligence() {
        let packages = [
            package("rusty-runtime", &["rusty-workspace", "rusty-code-analysis"]),
            package("rusty-workspace", &[]),
            package("rusty-code-analysis", &[]),
        ];
        let findings = check_layers(&packages);
        assert_eq!(findings.len(), 1, "only the code-analysis edge is wrong");
        assert!(findings[0].problem.contains("rusty-code-analysis"));
    }

    #[test]
    fn an_unclassified_package_is_reported() {
        let findings = check_layers(&[package("brand-new-crate", &[])]);
        assert!(findings[0].problem.contains("not assigned to a layer"));
    }
}
