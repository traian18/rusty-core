//! `check-readmes`: every crate explains why it exists and how it works, and its
//! "In the workspace" lists match the real dependencies.

use std::{collections::BTreeMap, collections::BTreeSet, fs, path::Path};

use crate::{report::Finding, workspace::Package};

/// Headings every crate README has, in this order, as the README template says.
const REQUIRED_SECTIONS: [&str; 5] = [
    "Why it exists",
    "How it works",
    "Public API",
    "In the workspace",
    "Must never",
];

pub fn check_readmes(root: &Path, packages: &[Package]) -> Vec<Finding> {
    let used_by = reverse_dependencies(packages);
    let mut findings = Vec::new();
    for package in packages {
        let readme_path = package.folder.join("README.md");
        match fs::read_to_string(&readme_path) {
            Ok(readme) => findings.extend(check_one(package, &readme, &used_by)),
            Err(_) => findings.push(Finding::new(
                &package.name,
                format!("no README.md in {}", package.relative_folder(root)),
            )),
        }
    }
    findings
}

fn check_one(
    package: &Package,
    readme: &str,
    used_by: &BTreeMap<&str, BTreeSet<&str>>,
) -> Vec<Finding> {
    let mut findings = missing_sections(package, readme);
    findings.extend(too_short(package, readme));
    findings.extend(stale_dependency_lists(package, readme, used_by));
    findings
}

/// A README under this many lines cannot explain why, how, key types and decisions.
const MINIMUM_LINES: usize = 40;

fn too_short(package: &Package, readme: &str) -> Option<Finding> {
    let lines = readme.lines().count();
    (lines < MINIMUM_LINES).then(|| {
        Finding::new(
            &package.name,
            format!(
                "README has {lines} lines; a crate README is detailed (at least {MINIMUM_LINES})"
            ),
        )
    })
}

fn missing_sections(package: &Package, readme: &str) -> Vec<Finding> {
    let headings = headings_of(readme);
    REQUIRED_SECTIONS
        .iter()
        .filter(|section| !headings.contains(section))
        .map(|section| {
            Finding::new(
                &package.name,
                format!("README has no '## {section}' section"),
            )
        })
        .collect()
}

fn stale_dependency_lists(
    package: &Package,
    readme: &str,
    used_by: &BTreeMap<&str, BTreeSet<&str>>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let actual_dependencies: BTreeSet<&str> =
        package.dependencies.iter().map(String::as_str).collect();
    let actual_users = used_by
        .get(package.name.as_str())
        .cloned()
        .unwrap_or_default();
    for (label, actual) in [
        ("Depends on", actual_dependencies),
        ("Used by", actual_users),
    ] {
        let Some(listed) = listed_names(readme, label) else {
            continue;
        };
        if listed != actual {
            findings.push(Finding::new(
                &package.name,
                format!(
                    "README '{label}' is out of date: {}",
                    difference(&listed, &actual)
                ),
            ));
        }
    }
    findings
}

/// `used_by["rusty-agent-core"]` = the packages that depend on it.
fn reverse_dependencies(packages: &[Package]) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut used_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for package in packages {
        for dependency in &package.dependencies {
            used_by.entry(dependency).or_default().insert(&package.name);
        }
    }
    used_by
}

fn headings_of(readme: &str) -> Vec<&str> {
    readme
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(str::trim)
        .collect()
}

/// The backticked names on the README's `- **<label>:**` line; `None` when there is no such line.
fn listed_names<'a>(readme: &'a str, label: &str) -> Option<BTreeSet<&'a str>> {
    let prefix = format!("- **{label}:**");
    let line = readme
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))?;
    Some(
        line.split('`')
            .skip(1)
            .step_by(2)
            .collect::<BTreeSet<&str>>(),
    )
}

fn difference(listed: &BTreeSet<&str>, actual: &BTreeSet<&str>) -> String {
    let missing: Vec<&str> = actual.difference(listed).copied().collect();
    let extra: Vec<&str> = listed.difference(actual).copied().collect();
    format!("missing {missing:?}, not a dependency {extra:?}")
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

    const COMPLETE: &str = "# x\n\n## Why it exists\n## How it works\n## Public API\n\
        ## In the workspace\n- **Depends on:** `a`\n- **Used by:** `b`\n## Must never\n";

    #[test]
    fn a_complete_readme_with_true_lists_has_no_findings() {
        let packages = [package("x", &["a"]), package("b", &["x"])];
        let used_by = reverse_dependencies(&packages);
        let detailed = format!("{COMPLETE}{}", "More explanation.\n".repeat(MINIMUM_LINES));
        assert!(check_one(&packages[0], &detailed, &used_by).is_empty());
    }

    #[test]
    fn a_short_readme_is_reported_even_when_complete() {
        let packages = [package("x", &["a"]), package("b", &["x"])];
        let used_by = reverse_dependencies(&packages);
        let problems = check_one(&packages[0], COMPLETE, &used_by);
        assert!(problems.iter().any(|f| f.problem.contains("is detailed")));
    }

    #[test]
    fn missing_sections_and_stale_lists_are_reported() {
        let packages = [package("x", &["a", "c"])];
        let used_by = reverse_dependencies(&packages);
        let readme = "# x\n\n## Why it exists\n- **Depends on:** `a`\n";
        let problems: Vec<String> = check_one(&packages[0], readme, &used_by)
            .into_iter()
            .map(|finding| finding.problem)
            .collect();
        assert!(problems.iter().any(|p| p.contains("'## How it works'")));
        assert!(problems
            .iter()
            .any(|p| p.contains("'Depends on' is out of date")));
    }
}
