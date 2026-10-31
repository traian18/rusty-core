//! Relations between infrastructure files: Terraform module directories (what is declared and
//! what is referenced) and Terragrunt units (dependencies and includes). Built once for the
//! repository from the files' text, then read by the rules that need more than one file.
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    doc::hcl::{self, Block},
    kind::InfraKind,
};

/// A declaration with the file and line it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    pub name: String,
    pub path: String,
    pub line: u32,
}

/// One Terraform module directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerraformModule {
    pub variables: Vec<Declared>,
    pub locals: Vec<Declared>,
    /// `type.name` of each data source.
    pub data: Vec<Declared>,
    /// `var.x`, `local.x` and `data.t.n` as written in code of this directory.
    pub references: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    /// Directory the dependency points at, normalised; `None` when it is not a plain path.
    pub target: Option<String>,
    pub written: String,
    pub line: u32,
}

/// One Terragrunt unit (a directory with a `terragrunt.hcl`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub path: String,
    pub dependencies: Vec<Dependency>,
    /// Files the unit includes, resolved; the first element is the nearest.
    pub includes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InfraGraph {
    /// By directory ("" for the repository root).
    pub modules: BTreeMap<String, TerraformModule>,
    /// By unit directory.
    pub units: BTreeMap<String, Unit>,
    /// Every file path of the repository, for existence checks.
    pub files: BTreeSet<String>,
    /// Include chains from `includes` of each file that is included: file → files it includes.
    pub include_edges: BTreeMap<String, Vec<String>>,
}

/// Counts for a repository overview.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InfraOverview {
    pub terraform_modules: usize,
    pub terragrunt_units: usize,
    pub unit_dependencies: usize,
    /// Terragrunt units that are part of a dependency loop.
    pub units_in_cycles: usize,
}

pub fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `rel` applied to `dir`, with `.` and `..` resolved; `None` when it leaves the repository.
pub fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if rel.starts_with('/') || dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn strip_comments(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && !t.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Names that follow `prefix` (`var.`) wherever it starts a reference.
fn after_prefix(text: &str, prefix: &str, out: &mut BTreeSet<String>) {
    let mut rest = text;
    let mut consumed = 0;
    while let Some(at) = rest.find(prefix) {
        let absolute = consumed + at;
        let before = text[..absolute].chars().next_back();
        let tail = &rest[at + prefix.len()..];
        let name: String = tail.chars().take_while(|c| is_ident(*c)).collect();
        if before.is_none_or(|c| !is_ident(c) && c != '.') && !name.is_empty() {
            out.insert(format!("{prefix}{name}"));
        }
        consumed = absolute + prefix.len();
        rest = &text[consumed..];
    }
}

fn references(text: &str) -> BTreeSet<String> {
    let code = strip_comments(text);
    let mut out = BTreeSet::new();
    after_prefix(&code, "var.", &mut out);
    after_prefix(&code, "local.", &mut out);
    // `data.type.name`
    let mut data = BTreeSet::new();
    after_prefix(&code, "data.", &mut data);
    let mut rest = code.as_str();
    while let Some(at) = rest.find("data.") {
        let tail = &rest[at + 5..];
        let ty: String = tail.chars().take_while(|c| is_ident(*c)).collect();
        let after = &tail[ty.len()..];
        if let Some(name_tail) = after.strip_prefix('.') {
            let name: String = name_tail.chars().take_while(|c| is_ident(*c)).collect();
            if !ty.is_empty() && !name.is_empty() {
                out.insert(format!("data.{ty}.{name}"));
            }
        }
        rest = &rest[at + 5..];
    }
    out
}

fn declare(module: &mut TerraformModule, path: &str, body: &[Block], attrs: &[hcl::Attr]) {
    let _ = attrs;
    for block in body {
        match block.kind.as_str() {
            "variable" => {
                if let Some(name) = block.labels.first() {
                    module.variables.push(Declared {
                        name: name.clone(),
                        path: path.to_owned(),
                        line: block.line,
                    });
                }
            }
            "locals" => {
                for attr in &block.body.attrs {
                    module.locals.push(Declared {
                        name: attr.name.clone(),
                        path: path.to_owned(),
                        line: attr.line,
                    });
                }
            }
            "data" => {
                if let (Some(ty), Some(name)) = (block.labels.first(), block.labels.get(1)) {
                    module.data.push(Declared {
                        name: format!("{ty}.{name}"),
                        path: path.to_owned(),
                        line: block.line,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Quoted strings inside an HCL list as written.
fn strings(value: &str) -> Vec<String> {
    value
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_owned())
        .collect()
}

/// The directory a Terragrunt `config_path` names, relative to the unit's directory.
fn config_target(unit_dir: &str, written: &str) -> Option<String> {
    if written.contains('$') {
        return None;
    }
    join(unit_dir, written)
}

/// Where `find_in_parent_folders(...)` resolves from `file`, among the repository's files.
fn find_in_parent(file: &str, name: &str, files: &BTreeSet<String>) -> Option<String> {
    let mut dir = dir_of(dir_of(file)).to_owned();
    let mut first = true;
    loop {
        let candidate = if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        };
        if files.contains(&candidate) {
            return Some(candidate);
        }
        if dir.is_empty() && !first {
            return None;
        }
        first = false;
        if dir.is_empty() {
            return None;
        }
        dir = dir_of(&dir).to_owned();
    }
}

fn include_target(file: &str, expression: &str, files: &BTreeSet<String>) -> Option<String> {
    if let Some(call) = expression.find("find_in_parent_folders(") {
        let args = &expression[call + "find_in_parent_folders(".len()..];
        let name = strings(args.split(')').next().unwrap_or(""))
            .into_iter()
            .next();
        let name = name.unwrap_or_else(|| "terragrunt.hcl".to_owned());
        // The modern root file is `root.hcl`; accept it when the default is absent.
        return find_in_parent(file, &name, files).or_else(|| {
            (name == "terragrunt.hcl")
                .then(|| find_in_parent(file, "root.hcl", files))
                .flatten()
        });
    }
    let literal = strings(expression).into_iter().next()?;
    if literal.contains('$') {
        return None;
    }
    join(dir_of(file), &literal)
}

impl InfraGraph {
    /// `files` are `(path, text)` of the infrastructure files; `all_paths` is every file in the
    /// repository (for existence checks).
    pub fn build(files: &[(&str, &str)], all_paths: &[String]) -> Self {
        let mut graph = InfraGraph {
            files: all_paths.iter().cloned().collect(),
            ..Self::default()
        };
        for (path, text) in files {
            let name = path.rsplit('/').next().unwrap_or(path);
            let kind = crate::kind::classify(path, text);
            match kind {
                Some(InfraKind::Terraform) => {
                    let module = graph.modules.entry(dir_of(path).to_owned()).or_default();
                    let parsed = hcl::parse(text);
                    declare(module, path, &parsed.body.blocks, &parsed.body.attrs);
                    module.references.extend(references(text));
                }
                Some(InfraKind::Terragrunt) => {
                    let parsed = hcl::parse(text);
                    let dir = dir_of(path).to_owned();
                    let mut unit = Unit {
                        path: (*path).to_owned(),
                        ..Unit::default()
                    };
                    for block in &parsed.body.blocks {
                        match block.kind.as_str() {
                            "dependency" => {
                                if let Some(attr) = block.body.attr("config_path") {
                                    let written = attr
                                        .literal()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| attr.value.clone());
                                    unit.dependencies.push(Dependency {
                                        target: attr.literal().and_then(|w| config_target(&dir, w)),
                                        written,
                                        line: attr.line,
                                    });
                                }
                            }
                            "dependencies" => {
                                if let Some(attr) = block.body.attr("paths") {
                                    for written in strings(&attr.value) {
                                        unit.dependencies.push(Dependency {
                                            target: config_target(&dir, &written),
                                            written,
                                            line: attr.line,
                                        });
                                    }
                                }
                            }
                            "include" => {
                                if let Some(attr) = block.body.attr("path") {
                                    if let Some(target) =
                                        include_target(path, &attr.value, &graph.files)
                                    {
                                        unit.includes.push(target);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if !unit.includes.is_empty() {
                        graph
                            .include_edges
                            .insert((*path).to_owned(), unit.includes.clone());
                    }
                    // Only `terragrunt.hcl` files are units; included files such as `root.hcl`
                    // contribute include edges but no dependencies of their own.
                    if name == "terragrunt.hcl" {
                        graph.units.insert(dir, unit);
                    }
                }
                _ => {}
            }
        }
        graph
    }

    /// Longest chain of includes starting at `file` (the file itself is depth 0).
    pub fn include_depth(&self, file: &str) -> u32 {
        fn walk(graph: &InfraGraph, file: &str, seen: &mut Vec<String>) -> u32 {
            if seen.iter().any(|s| s == file) {
                return 0;
            }
            seen.push(file.to_owned());
            let deeper = graph
                .include_edges
                .get(file)
                .map(|targets| {
                    targets
                        .iter()
                        .map(|t| 1 + walk(graph, t, seen))
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            seen.pop();
            deeper
        }
        walk(self, file, &mut Vec::new())
    }

    pub fn overview(&self) -> InfraOverview {
        let edges: BTreeMap<String, Vec<String>> = self
            .units
            .iter()
            .map(|(dir, unit)| {
                let targets = unit
                    .dependencies
                    .iter()
                    .filter_map(|d| d.target.clone())
                    .collect();
                (dir.clone(), targets)
            })
            .collect();
        InfraOverview {
            terraform_modules: self.modules.len(),
            terragrunt_units: self.units.len(),
            unit_dependencies: self.units.values().map(|u| u.dependencies.len()).sum(),
            units_in_cycles: cycles(&edges).iter().map(BTreeSet::len).sum(),
        }
    }

    pub fn has_files_under(&self, dir: &str) -> bool {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        self.files.iter().any(|f| f.starts_with(&prefix))
    }
}

/// Strongly connected components with a cycle: more than one node, or a node that depends on
/// itself (Tarjan, iterative enough for the sizes involved).
pub fn cycles(edges: &BTreeMap<String, Vec<String>>) -> Vec<BTreeSet<String>> {
    struct State<'a> {
        edges: &'a BTreeMap<String, Vec<String>>,
        index: BTreeMap<&'a str, usize>,
        low: BTreeMap<&'a str, usize>,
        on_stack: BTreeSet<&'a str>,
        stack: Vec<&'a str>,
        next: usize,
        found: Vec<BTreeSet<String>>,
    }
    fn visit<'a>(state: &mut State<'a>, node: &'a str) {
        state.index.insert(node, state.next);
        state.low.insert(node, state.next);
        state.next += 1;
        state.stack.push(node);
        state.on_stack.insert(node);
        let targets: &'a [String] = state.edges.get(node).map_or(&[], Vec::as_slice);
        for target in targets {
            let target = target.as_str();
            if !state.edges.contains_key(target) {
                continue;
            }
            if !state.index.contains_key(target) {
                visit(state, target);
                let low = state.low[node].min(state.low[target]);
                state.low.insert(node, low);
            } else if state.on_stack.contains(target) {
                let low = state.low[node].min(state.index[target]);
                state.low.insert(node, low);
            }
        }
        if state.low[node] == state.index[node] {
            let mut component = BTreeSet::new();
            while let Some(top) = state.stack.pop() {
                state.on_stack.remove(top);
                component.insert(top.to_owned());
                if top == node {
                    break;
                }
            }
            let self_loop = targets.iter().any(|t| t == node);
            if component.len() > 1 || self_loop {
                state.found.push(component);
            }
        }
    }
    let mut state = State {
        edges,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        on_stack: BTreeSet::new(),
        stack: Vec::new(),
        next: 0,
        found: Vec::new(),
    };
    for node in edges.keys() {
        if !state.index.contains_key(node.as_str()) {
            visit(&mut state, node);
        }
    }
    state.found
}
