//! `go.mod` read as text: the module path becomes the package name and `require` lines its
//! dependencies (`// indirect` ones are marked as not declared directly). Nothing is executed.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    ManifestAdapter, ManifestInput, ManifestParse, Package,
};

pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![Arc::new(GoMod)]
}

struct GoMod;

impl ManifestAdapter for GoMod {
    fn id(&self) -> &'static str {
        "go-mod"
    }

    fn file_name(&self) -> &'static str {
        "go.mod"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let (mut module, mut direct, mut indirect) = (None, Vec::new(), Vec::new());
        let mut in_require = false;
        for raw in input.text.lines() {
            let (code, comment) = raw.split_once("//").unwrap_or((raw, ""));
            let line = code.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("module ") {
                module = Some(rest.trim().trim_matches('"').to_owned());
            } else if line == "require (" {
                in_require = true;
            } else if line == ")" {
                in_require = false;
            } else if let Some(rest) = line
                .strip_prefix("require ")
                .filter(|r| !r.starts_with('('))
            {
                push(rest, comment, &mut direct, &mut indirect);
            } else if in_require {
                push(line, comment, &mut direct, &mut indirect);
            }
        }
        let Some(module) = module else {
            parse.diagnostics.push(
                harness_workspace::repository_intelligence::DiscoveryDiagnostic::for_path(
                    input.path,
                    "go.mod has no module line",
                ),
            );
            return parse;
        };
        let mut all: Vec<String> = direct.iter().chain(&indirect).cloned().collect();
        all.sort();
        all.dedup();
        let mut package = Package::declared(module, input.path.clone(), all);
        // Indirect requirements are not declared by this module's own code.
        package.facts.dev_dependencies = indirect
            .into_iter()
            .filter(|d| !direct.contains(d))
            .collect();
        package.facts.dev_dependencies.sort();
        package.facts.dev_dependencies.dedup();
        parse.packages.push(package);
        parse
    }
}

fn push(line: &str, comment: &str, direct: &mut Vec<String>, indirect: &mut Vec<String>) {
    let Some(path) = line.split_whitespace().next() else {
        return;
    };
    if comment.contains("indirect") {
        indirect.push(path.to_owned());
    } else {
        direct.push(path.to_owned());
    }
}
