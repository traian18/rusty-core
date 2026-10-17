//! Next.js: file-system routes of the App Router (`app/**/page`, `app/**/route`) and the Pages
//! Router (`pages/**`). The path comes from the directory convention; route handler methods are
//! read from the file's exports.
use std::path::Path;

use harness_workspace::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationEvidence, RelationKind,
    },
    import_graph::ByteSpan,
    repository_intelligence::{DiscoveryError, Package, RootRelativePath},
};
use tokio_util::sync::CancellationToken;
use tree_sitter::Node;

use crate::syntax::{is_script, parse_script, text, walk};

const ID: &str = "nextjs";
const VERSION: u32 = 1;
const APP_RULE: &str = "nextjs-app-route";
const PAGES_RULE: &str = "nextjs-pages-route";
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

pub struct NextjsAdapter;

impl FrameworkAdapter for NextjsAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "Next.js".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "App Router: app/**/page.* (PAGE) and app/**/route.* (one route per exported GET, POST, PUT, PATCH, DELETE, HEAD or OPTIONS), also under src/".into(),
                "Pages Router: pages/** (PAGE) and pages/api/** (API); _app, _document and _error are skipped, also under src/".into(),
                "route groups (x) are dropped; [id] becomes :id, [...slug] becomes *slug, [[...slug]] becomes *slug?".into(),
            ],
            unsupported: vec![
                "parallel (@slot) and intercepting ((.)x) routes are reported, no relation".into(),
                "rewrites, redirects and middleware in next.config or middleware files".into(),
                "custom pageExtensions".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "next")
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        let base = FrameworkInput::package_dir(package);
        for indexed in input.files_of(package) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            let file = &indexed.path;
            if !is_script(file) {
                continue;
            }
            let Ok(relative) = file.as_path().strip_prefix(&base) else {
                continue;
            };
            if let Some(rest) = strip_roots(relative, &["app", "src/app"]) {
                app_route(package, file, rest, input, output);
            } else if let Some(rest) = strip_roots(relative, &["pages", "src/pages"]) {
                pages_route(package, file, rest, output);
            }
        }
        Ok(())
    }
}

fn strip_roots<'a>(path: &'a Path, roots: &[&str]) -> Option<&'a Path> {
    roots.iter().find_map(|root| path.strip_prefix(root).ok())
}

enum Segment {
    Part(String),
    /// A segment the adapter cannot turn into a path.
    Unsupported(String),
}

fn segment(name: &str) -> Option<Segment> {
    if name.starts_with('@') || name.starts_with("(.") {
        return Some(Segment::Unsupported(name.to_owned()));
    }
    if name.starts_with('(') && name.ends_with(')') {
        return None;
    }
    let part = if let Some(inner) = name
        .strip_prefix("[[...")
        .and_then(|n| n.strip_suffix("]]"))
    {
        format!("*{inner}?")
    } else if let Some(inner) = name.strip_prefix("[...").and_then(|n| n.strip_suffix(']')) {
        format!("*{inner}")
    } else if let Some(inner) = name.strip_prefix('[').and_then(|n| n.strip_suffix(']')) {
        format!(":{inner}")
    } else {
        name.to_owned()
    };
    Some(Segment::Part(part))
}

/// `Some(path)` for the directories above a file, or `None` after reporting an unsupported segment.
fn route_path(
    directories: &Path,
    file: &RootRelativePath,
    output: &mut FrameworkOutput,
) -> Option<String> {
    let mut parts = Vec::new();
    for component in directories.components() {
        let name = component.as_os_str().to_string_lossy();
        match segment(&name) {
            Some(Segment::Part(part)) => parts.push(part),
            Some(Segment::Unsupported(name)) => {
                output.unresolved(
                    file,
                    format!(
                        "`{name}` is a parallel or intercepting route segment; no relation emitted"
                    ),
                );
                return None;
            }
            None => {}
        }
    }
    Some(format!("/{}", parts.join("/")))
}

fn convention_evidence(file: &RootRelativePath, what: &str) -> RelationEvidence {
    RelationEvidence::new(
        file,
        ByteSpan { start: 0, end: 0 },
        &format!("{what}: {}", file.as_path().display()),
    )
}

fn relate(
    package: &Package,
    rule: &'static str,
    file: &RootRelativePath,
    label: String,
    symbol: Option<String>,
    evidence: RelationEvidence,
    output: &mut FrameworkOutput,
) {
    output.relate(FrameworkRelation::new(
        ID,
        Some(&package.id),
        RelationDraft {
            kind: RelationKind::RouteRegisteredTo,
            rule,
            rule_version: VERSION,
            certainty: Certainty::Convention,
            from: file.clone(),
            to: Some(file.clone()),
            from_symbol: None,
            to_symbol: symbol,
            label,
            evidence,
        },
    ));
}

fn app_route(
    package: &Package,
    file: &RootRelativePath,
    rest: &Path,
    input: &FrameworkInput<'_>,
    output: &mut FrameworkOutput,
) {
    let Some(stem) = rest.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    if !matches!(stem, "page" | "route") {
        return;
    }
    let Some(path) = route_path(rest.parent().unwrap_or(Path::new("")), file, output) else {
        return;
    };
    if stem == "page" {
        relate(
            package,
            APP_RULE,
            file,
            format!("PAGE {path}"),
            Some("default".into()),
            convention_evidence(file, "App Router page"),
            output,
        );
        return;
    }
    let Some(source) = input.read(file) else {
        return;
    };
    let Some(tree) = parse_script(file, &source) else {
        return;
    };
    let mut found = Vec::new();
    walk(tree.root_node(), &mut |node| {
        if node.kind() != "export_statement" {
            return;
        }
        for name in exported_names(node, &source) {
            if METHODS.contains(&name.as_str()) {
                found.push((name, node));
            }
        }
    });
    if found.is_empty() {
        output.unresolved(file, "route file exports no GET, POST, PUT, PATCH, DELETE, HEAD or OPTIONS handler; no relation emitted");
    }
    for (method, node) in found {
        let evidence = RelationEvidence::new(
            file,
            ByteSpan {
                start: node.start_byte(),
                end: node.end_byte(),
            },
            text(node, &source),
        );
        relate(
            package,
            APP_RULE,
            file,
            format!("{method} {path}"),
            Some(method),
            evidence,
            output,
        );
    }
}

fn pages_route(
    package: &Package,
    file: &RootRelativePath,
    rest: &Path,
    output: &mut FrameworkOutput,
) {
    let Some(stem) = rest.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let name = rest.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if stem.starts_with('_')
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with(".d.ts")
    {
        return;
    }
    let mut directories = rest.parent().unwrap_or(Path::new("")).to_path_buf();
    if stem != "index" {
        directories.push(stem);
    }
    let Some(path) = route_path(&directories, file, output) else {
        return;
    };
    let is_api = path == "/api" || path.starts_with("/api/");
    let label = if is_api {
        format!("API {path}")
    } else {
        format!("PAGE {path}")
    };
    relate(
        package,
        PAGES_RULE,
        file,
        label,
        Some("default".into()),
        convention_evidence(file, "Pages Router file"),
        output,
    );
}

/// Names an `export` statement exposes: `export function X`, `export const X = …`, `export { X, Y }`.
fn exported_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(declaration) = node.child_by_field_name("declaration") {
        match declaration.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = declaration.child_by_field_name("name") {
                    names.push(text(name, source).to_owned());
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut cursor = declaration.walk();
                for declarator in declaration.children(&mut cursor) {
                    if let Some(name) = declarator.child_by_field_name("name") {
                        names.push(text(name, source).to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "export_clause" {
            let mut inner = child.walk();
            for specifier in child.children(&mut inner) {
                if specifier.kind() == "export_specifier" {
                    let exported = specifier
                        .child_by_field_name("alias")
                        .or_else(|| specifier.child_by_field_name("name"));
                    if let Some(exported) = exported {
                        names.push(text(exported, source).to_owned());
                    }
                }
            }
        }
    }
    names
}
