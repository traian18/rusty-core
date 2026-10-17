//! `*.csproj` read as text: the assembly or file name, `PackageReference` and `ProjectReference`
//! items, and whether the project uses the Web SDK. MSBuild is never run.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    ManifestAdapter, ManifestInput, ManifestParse, Package,
};

pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![Arc::new(Csproj)]
}

/// The value of `name="…"` inside a tag's text.
pub(crate) fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let start = tag.find(&marker)? + marker.len();
    tag[start..].split('"').next()
}

/// The text of every `<element …>` opening or self-closing tag named `element`.
fn tags<'a>(xml: &'a str, element: &str) -> Vec<&'a str> {
    let open = format!("<{element}");
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(position) = xml[index..].find(&open) {
        let start = index + position;
        let after = &xml[start + open.len()..];
        if after.starts_with(|c: char| c.is_alphanumeric()) {
            index = start + open.len();
            continue;
        }
        let end = xml[start..].find('>').map_or(xml.len(), |i| start + i + 1);
        found.push(&xml[start..end]);
        index = end;
    }
    found
}

struct Csproj;

impl ManifestAdapter for Csproj {
    fn id(&self) -> &'static str {
        "csproj"
    }

    fn file_name(&self) -> &'static str {
        "*.csproj"
    }

    fn handles(&self, file_name: &str) -> bool {
        file_name.ends_with(".csproj")
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let xml = input.text;
        if !xml.contains("<Project") {
            parse.diagnostics.push(
                harness_workspace::repository_intelligence::DiscoveryDiagnostic::for_path(
                    input.path,
                    "csproj has no <Project> element",
                ),
            );
            return parse;
        }
        let stem = input
            .path
            .as_path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_owned();
        let name = xml
            .split("<AssemblyName>")
            .nth(1)
            .and_then(|rest| rest.split("</AssemblyName>").next())
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty() && !n.contains('$'))
            .unwrap_or(stem);
        let (mut runtime, mut dev) = (Vec::new(), Vec::new());
        for tag in tags(xml, "PackageReference") {
            let Some(id) = attribute(tag, "Include").or_else(|| attribute(tag, "Update")) else {
                continue;
            };
            if attribute(tag, "PrivateAssets") == Some("all") {
                dev.push(id.to_owned());
            } else {
                runtime.push(id.to_owned());
            }
        }
        for tag in tags(xml, "ProjectReference") {
            if let Some(include) = attribute(tag, "Include") {
                let target = include
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(include)
                    .trim_end_matches(".csproj");
                runtime.push(format!("project:{target}"));
            }
        }
        // The Web SDK brings ASP.NET Core without naming a package.
        if tags(xml, "Project")
            .iter()
            .any(|t| attribute(t, "Sdk") == Some("Microsoft.NET.Sdk.Web"))
        {
            runtime.push("Microsoft.NET.Sdk.Web".to_owned());
        }
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
