//! Python project files read as text: `pyproject.toml` (PEP 621, Poetry, dependency groups),
//! `setup.cfg`, `Pipfile` and `requirements*.txt`. Nothing is executed and `setup.py` is not read.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    DiscoveryDiagnostic, ManifestAdapter, ManifestInput, ManifestParse, Package,
};

use crate::stdlib::normalize;

/// Every manifest adapter this crate provides.
pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![
        Arc::new(PyProject),
        Arc::new(SetupCfg),
        Arc::new(Pipfile),
        Arc::new(Requirements),
    ]
}

/// The distribution name from a PEP 508 requirement line: `Django[argon2]>=4 ; python_version>"3.8"`.
pub(crate) fn requirement_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
        return None;
    }
    if let Some((_, egg)) = line.split_once("#egg=") {
        return Some(normalize(egg.split(['&', ' ']).next()?));
    }
    let end = line
        .find(|c: char| " ;[<>=!~(@".contains(c))
        .unwrap_or(line.len());
    let name = &line[..end];
    (!name.is_empty() && !name.contains('/') && !name.contains(':')).then(|| normalize(name))
}

fn directory_name(input: &ManifestInput<'_>) -> String {
    input
        .path
        .directory()
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .or_else(|| {
            input
                .root
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "project".into())
}

fn package(
    input: &ManifestInput<'_>,
    name: String,
    runtime: Vec<String>,
    dev: Vec<String>,
) -> Package {
    let runtime: std::collections::BTreeSet<String> = runtime.into_iter().collect();
    let dev: std::collections::BTreeSet<String> =
        dev.into_iter().filter(|d| !runtime.contains(d)).collect();
    let all: Vec<String> = runtime.iter().chain(dev.iter()).cloned().collect();
    let mut package = Package::declared(name, input.path.clone(), all);
    // A dependency declared both ways counts as a regular dependency.
    package.facts.dev_dependencies = dev.into_iter().collect();
    package
}

struct PyProject;

impl ManifestAdapter for PyProject {
    fn id(&self) -> &'static str {
        "pyproject"
    }
    fn file_name(&self) -> &'static str {
        "pyproject.toml"
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let value: toml::Value = match toml::from_str(input.text) {
            Ok(value) => value,
            Err(error) => {
                parse.diagnostics.push(DiscoveryDiagnostic::for_path(
                    input.path,
                    format!("pyproject.toml is not valid TOML: {error}"),
                ));
                return parse;
            }
        };
        let project = value.get("project");
        let poetry = value.get("tool").and_then(|t| t.get("poetry"));
        let Some(name) = project
            .and_then(|p| p.get("name"))
            .or_else(|| poetry.and_then(|p| p.get("name")))
            .and_then(|n| n.as_str())
        else {
            return parse; // only tool configuration, no package
        };
        let (mut runtime, mut dev) = (Vec::new(), Vec::new());
        let strings = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|d| d.as_str())
                .filter_map(requirement_name)
                .collect()
        };
        runtime.extend(strings(project.and_then(|p| p.get("dependencies"))));
        if let Some(optional) = project
            .and_then(|p| p.get("optional-dependencies"))
            .and_then(|o| o.as_table())
        {
            for (_, list) in optional {
                dev.extend(strings(Some(list)));
            }
        }
        if let Some(groups) = value.get("dependency-groups").and_then(|g| g.as_table()) {
            for (_, list) in groups {
                dev.extend(strings(Some(list)));
            }
        }
        dev.extend(strings(
            value.get("build-system").and_then(|b| b.get("requires")),
        ));
        let keys = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.as_table())
                .into_iter()
                .flatten()
                .filter(|(k, _)| k.as_str() != "python")
                .map(|(k, _)| normalize(k))
                .collect()
        };
        runtime.extend(keys(poetry.and_then(|p| p.get("dependencies"))));
        dev.extend(keys(poetry.and_then(|p| p.get("dev-dependencies"))));
        if let Some(groups) = poetry
            .and_then(|p| p.get("group"))
            .and_then(|g| g.as_table())
        {
            for (_, group) in groups {
                dev.extend(keys(group.get("dependencies")));
            }
        }
        parse
            .packages
            .push(package(input, name.to_owned(), runtime, dev));
        parse
    }
}

struct SetupCfg;

impl ManifestAdapter for SetupCfg {
    fn id(&self) -> &'static str {
        "setup-cfg"
    }
    fn file_name(&self) -> &'static str {
        "setup.cfg"
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        // A pyproject.toml next to it that names the package takes precedence.
        if has_named_pyproject(input) {
            return parse;
        }
        let (mut section, mut key) = (String::new(), String::new());
        let (mut name, mut runtime, mut dev) = (None, Vec::new(), Vec::new());
        for raw in input.text.lines() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with(['#', ';']) {
                continue;
            }
            if let Some(header) = line
                .trim()
                .strip_prefix('[')
                .and_then(|l| l.strip_suffix(']'))
            {
                section = header.to_owned();
                key.clear();
                continue;
            }
            let continuation = raw.starts_with([' ', '\t']);
            if !continuation {
                if let Some((k, v)) = line.split_once(['=', ':']) {
                    key = k.trim().to_owned();
                    let value = v.trim();
                    if section == "metadata" && key == "name" {
                        name = Some(value.to_owned());
                    }
                    push_requirement(&section, &key, value, &mut runtime, &mut dev);
                }
            } else {
                push_requirement(&section, &key, line.trim(), &mut runtime, &mut dev);
            }
        }
        if let Some(name) = name {
            parse.packages.push(package(input, name, runtime, dev));
        }
        parse
    }
}

fn push_requirement(
    section: &str,
    key: &str,
    value: &str,
    runtime: &mut Vec<String>,
    dev: &mut Vec<String>,
) {
    let Some(name) = requirement_name(value) else {
        return;
    };
    match (section, key) {
        ("options", "install_requires") => runtime.push(name),
        ("options", "setup_requires" | "tests_require") | ("options.extras_require", _) => {
            dev.push(name)
        }
        _ => {}
    }
}

fn has_named_pyproject(input: &ManifestInput<'_>) -> bool {
    let path = input
        .root
        .join(input.path.directory())
        .join("pyproject.toml");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .is_some_and(|value| {
            value.get("project").and_then(|p| p.get("name")).is_some()
                || value
                    .get("tool")
                    .and_then(|t| t.get("poetry"))
                    .and_then(|p| p.get("name"))
                    .is_some()
        })
}

struct Pipfile;

impl ManifestAdapter for Pipfile {
    fn id(&self) -> &'static str {
        "pipfile"
    }
    fn file_name(&self) -> &'static str {
        "Pipfile"
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        if has_named_pyproject(input) {
            return parse;
        }
        let value: toml::Value = match toml::from_str(input.text) {
            Ok(value) => value,
            Err(error) => {
                parse.diagnostics.push(DiscoveryDiagnostic::for_path(
                    input.path,
                    format!("Pipfile is not valid TOML: {error}"),
                ));
                return parse;
            }
        };
        let keys = |section: &str| -> Vec<String> {
            value
                .get(section)
                .and_then(|v| v.as_table())
                .into_iter()
                .flatten()
                .map(|(k, _)| normalize(k))
                .collect()
        };
        parse.packages.push(package(
            input,
            directory_name(input),
            keys("packages"),
            keys("dev-packages"),
        ));
        parse
    }
}

/// `requirements.txt`, `requirements-dev.txt`, …: a package only when no richer project file sits next to it.
struct Requirements;

impl ManifestAdapter for Requirements {
    fn id(&self) -> &'static str {
        "requirements-txt"
    }
    fn file_name(&self) -> &'static str {
        "requirements.txt"
    }
    fn handles(&self, file_name: &str) -> bool {
        file_name.starts_with("requirements") && file_name.ends_with(".txt")
    }
    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let directory = input.root.join(input.path.directory());
        if has_named_pyproject(input)
            || ["setup.cfg", "Pipfile"]
                .iter()
                .any(|f| directory.join(f).exists())
        {
            return parse;
        }
        let name = input
            .path
            .as_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        // The plain file is the project; `requirements-dev.txt` and friends only add development tools.
        if name != "requirements.txt" && directory.join("requirements.txt").exists() {
            return parse;
        }
        let dependencies: Vec<String> = input.text.lines().filter_map(requirement_name).collect();
        let dev_file = name != "requirements.txt";
        let (runtime, dev) = if dev_file {
            (Vec::new(), dependencies)
        } else {
            (dependencies, Vec::new())
        };
        parse
            .packages
            .push(package(input, directory_name(input), runtime, dev));
        parse
    }
}
