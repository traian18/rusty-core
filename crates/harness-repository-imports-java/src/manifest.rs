//! Maven and Gradle files read as text: `pom.xml` (coordinates, dependencies, scopes) and
//! `build.gradle(.kts)` (dependency declarations by configuration). Neither tool is run.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    ManifestAdapter, ManifestInput, ManifestParse, Package,
};

pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![
        Arc::new(Pom),
        Arc::new(Gradle {
            file: "build.gradle",
        }),
        Arc::new(Gradle {
            file: "build.gradle.kts",
        }),
    ]
}

/// Leaf elements of an XML document with the path of their ancestors: `project/dependencies/dependency/artifactId`.
/// Comments, CDATA, processing instructions and attributes are skipped; this is not a general parser.
pub(crate) fn leaves(xml: &str) -> Vec<(String, String)> {
    let bytes = xml.as_bytes();
    let (mut index, mut stack, mut found) = (0usize, Vec::<String>::new(), Vec::new());
    let mut text_start = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let rest = &xml[index..];
        if rest.starts_with("<!--") {
            index += rest.find("-->").map_or(rest.len(), |i| i + 3);
            text_start = index;
            continue;
        }
        if rest.starts_with("<![CDATA[") || rest.starts_with("<?") || rest.starts_with("<!") {
            index += rest.find('>').map_or(rest.len(), |i| i + 1);
            text_start = index;
            continue;
        }
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[1..end];
        if let Some(name) = tag.strip_prefix('/') {
            if stack.last().is_some_and(|top| top == name.trim()) {
                let path = stack.join("/");
                let value = xml[text_start..index].trim();
                if !value.is_empty() {
                    found.push((path, value.to_owned()));
                }
                stack.pop();
            }
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap_or("").to_owned());
        }
        index += end + 1;
        text_start = index;
    }
    found
}

struct Pom;

impl ManifestAdapter for Pom {
    fn id(&self) -> &'static str {
        "maven-pom"
    }

    fn file_name(&self) -> &'static str {
        "pom.xml"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let leaves = leaves(input.text);
        let value = |path: &str| {
            leaves
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, v)| v.clone())
        };
        let Some(artifact) = value("project/artifactId") else {
            parse.diagnostics.push(
                harness_workspace::repository_intelligence::DiscoveryDiagnostic::for_path(
                    input.path,
                    "pom.xml names no artifactId",
                ),
            );
            return parse;
        };
        // Dependencies are consecutive leaves: groupId, artifactId, (version), (scope), in any order.
        let (mut runtime, mut dev) = (Vec::new(), Vec::new());
        let (mut group, mut artifact_id, mut scope) =
            (None::<String>, None::<String>, None::<String>);
        let mut flush = |group: &mut Option<String>,
                         artifact_id: &mut Option<String>,
                         scope: &mut Option<String>| {
            if let (Some(g), Some(a)) = (group.take(), artifact_id.take()) {
                let coordinate = format!("{g}:{a}");
                if matches!(scope.as_deref(), Some("test" | "provided")) {
                    dev.push(coordinate);
                } else {
                    runtime.push(coordinate);
                }
            }
            *scope = None;
        };
        for (path, text) in &leaves {
            let Some(field) = path.strip_prefix("project/dependencies/dependency/") else {
                continue;
            };
            match field {
                "groupId" => {
                    if group.is_some() && artifact_id.is_some() {
                        flush(&mut group, &mut artifact_id, &mut scope);
                    }
                    group = Some(text.clone());
                }
                "artifactId" => artifact_id = Some(text.clone()),
                "scope" => scope = Some(text.clone()),
                _ => {}
            }
            if group.is_some() && artifact_id.is_some() && field == "scope" {
                flush(&mut group, &mut artifact_id, &mut scope);
            }
        }
        flush(&mut group, &mut artifact_id, &mut scope);
        let mut all: Vec<String> = runtime.iter().chain(&dev).cloned().collect();
        all.sort();
        all.dedup();
        let mut package = Package::declared(artifact, input.path.clone(), all);
        package.facts.dev_dependencies = dev.into_iter().filter(|d| !runtime.contains(d)).collect();
        package.facts.dev_dependencies.sort();
        package.facts.dev_dependencies.dedup();
        parse.packages.push(package);
        parse
    }
}

struct Gradle {
    file: &'static str,
}

const RUNTIME: [&str; 4] = ["implementation", "api", "runtimeOnly", "compile"];
const DEVELOPMENT: [&str; 8] = [
    "testImplementation",
    "testRuntimeOnly",
    "testCompileOnly",
    "compileOnly",
    "annotationProcessor",
    "kapt",
    "developmentOnly",
    "testAnnotationProcessor",
];

impl ManifestAdapter for Gradle {
    fn id(&self) -> &'static str {
        "gradle-build"
    }

    fn file_name(&self) -> &'static str {
        self.file
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let directory = input.root.join(input.path.directory());
        // A pom.xml in the same directory is the project; Gradle files beside it add nothing.
        if directory.join("pom.xml").exists() {
            return parse;
        }
        // The Groovy file wins over the Kotlin one when both exist.
        if self.file == "build.gradle.kts" && directory.join("build.gradle").exists() {
            return parse;
        }
        let (mut runtime, mut dev) = (Vec::new(), Vec::new());
        for line in input.text.lines() {
            let line = line.trim();
            let Some(configuration) = line
                .split(|c: char| !c.is_alphanumeric())
                .next()
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let is_runtime = RUNTIME.contains(&configuration);
            if !is_runtime && !DEVELOPMENT.contains(&configuration) {
                continue;
            }
            let rest = line[configuration.len()..].trim_start_matches(['(', ' ']);
            let coordinate = if let Some(project) = rest.strip_prefix("project(") {
                let name = project.trim_matches(|c| matches!(c, '\'' | '"' | ':' | ')' | ' '));
                Some(format!("project:{name}"))
            } else {
                let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'));
                quote
                    .and_then(|q| rest[1..].split(q).next())
                    .map(|c| c.split(':').take(2).collect::<Vec<_>>().join(":"))
                    .filter(|c| c.contains(':'))
            };
            if let Some(coordinate) = coordinate {
                if is_runtime {
                    runtime.push(coordinate)
                } else {
                    dev.push(coordinate)
                }
            }
        }
        let name = std::fs::read_to_string(directory.join("settings.gradle"))
            .or_else(|_| std::fs::read_to_string(directory.join("settings.gradle.kts")))
            .ok()
            .and_then(|settings| {
                settings.lines().find_map(|l| {
                    let l = l.trim();
                    l.strip_prefix("rootProject.name").map(|r| {
                        r.trim_start_matches([' ', '='])
                            .trim()
                            .trim_matches(['\'', '"'])
                            .to_owned()
                    })
                })
            })
            .or_else(|| {
                input
                    .path
                    .directory()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "project".into());
        let mut all: Vec<String> = runtime.iter().chain(&dev).cloned().collect();
        all.sort();
        all.dedup();
        let mut package = Package::declared(name, input.path.clone(), all);
        package.facts.dev_dependencies = dev.into_iter().filter(|d| !runtime.contains(d)).collect();
        package.facts.dev_dependencies.sort();
        package.facts.dev_dependencies.dedup();
        parse.packages.push(package);
        parse
    }
}
