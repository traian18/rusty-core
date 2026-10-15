//! Rust: `mod`/`use` module paths over the file layout and inline modules, associated functions,
//! `impl` blocks, closures and struct-literal typed locals. Traits, macros and generics stay
//! unresolved.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, ImportHit, ImportLookup, ModuleHit, ModuleLookup,
    ProjectIndex, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Rust;

impl Syntax for Rust {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves `mod` declarations by file layout and inline `mod`, `use` paths with `crate`/`self`/`super`, aliases, groups and `pub use` re-exports".into(),
            "supports free functions, associated functions and methods in `impl` blocks, `Self`/`self` calls, closures, and receivers typed by struct literals or annotations".into(),
            "trait method dispatch, generics, `dyn`/`impl Trait`, macro-generated code, and calls inside macro invocations are not resolved".into(),
            "visibility (`pub`) and `cfg` attributes are not evaluated".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "use_declaration" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
                    use_tree(argument, &[], c, start, end);
                }
                Action::Skip
            }
            "mod_item" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Skip;
                };
                if node.child_by_field_name("body").is_none() {
                    return Action::Skip;
                }
                let full = if c.namespace().is_empty() {
                    c.text(name).to_owned()
                } else {
                    format!("{}::{}", c.namespace(), c.text(name))
                };
                Action::Namespace(full)
            }
            "struct_item" | "enum_item" | "union_item" | "trait_item" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Descend;
                };
                Action::Type(TypeSpec {
                    name: c.text(name).to_owned(),
                    name_range: (name.start_byte(), name.end_byte()),
                    kind: match node.kind() {
                        "enum_item" => TypeKind::Enum,
                        "trait_item" => TypeKind::Trait,
                        _ => TypeKind::Struct,
                    },
                    bases: Vec::new(),
                })
            }
            "impl_item" => match node
                .child_by_field_name("type")
                .and_then(|t| simple_type(c.text(t)))
            {
                Some(name) => Action::Container(last_segment(&name).to_owned()),
                None => Action::Descend,
            },
            "function_item" => {
                let name = node.child_by_field_name("name");
                let in_impl = c.current_type().is_some() && !c.in_callable();
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: if in_impl {
                        CallableKind::Method
                    } else {
                        CallableKind::Function
                    },
                    container: None,
                    namespace: None,
                    params: fn_params(node, c),
                })
            }
            "closure_expression" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: closure_params(node, c),
            }),
            "let_declaration" => {
                self.let_binding(node, c);
                Action::Descend
            }
            "for_expression" | "let_condition" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let mut names = Vec::new();
                    binding_names(pattern, c.src, &mut names);
                    for name in names {
                        c.declare(&name, false);
                    }
                }
                Action::Descend
            }
            "macro_invocation" => Action::Skip,
            "call_expression" => {
                self.call(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn rust_type(text: &str) -> Option<String> {
    let mut text = text.trim().trim_start_matches('&').trim();
    if let Some(rest) = text.strip_prefix('\'') {
        text = rest.split_once(' ').map_or("", |(_, rest)| rest).trim();
    }
    let text = text.strip_prefix("mut ").unwrap_or(text).trim();
    simple_type(text)
}

fn fn_params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            continue;
        }
        let ty = parameter
            .child_by_field_name("type")
            .and_then(|t| rust_type(c.text(t)));
        if let Some(pattern) = parameter.child_by_field_name("pattern") {
            let mut names = Vec::new();
            binding_names(pattern, c.src, &mut names);
            out.extend(names.into_iter().map(|n| (n, ty.clone())));
        }
    }
    out
}

fn closure_params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    binding_names(parameters, c.src, &mut names);
    names.into_iter().map(|n| (n, None)).collect()
}

fn use_tree(node: Node<'_>, prefix: &[String], c: &mut Collector<'_>, start: u32, end: u32) {
    let emit = |c: &mut Collector<'_>, segments: Vec<String>, alias: Option<String>| {
        let Some((last, module)) = segments.split_last() else {
            return;
        };
        if last == "self" {
            // `use a::b::{self}` binds the module `b`.
            if let Some(name) = module.last() {
                c.import(ImportFact {
                    local: alias.unwrap_or_else(|| name.clone()),
                    imported: String::new(),
                    module: module.join("::"),
                    kind: ImportKind::Namespace,
                    type_only: false,
                    start,
                    end,
                });
            }
            return;
        }
        c.import(ImportFact {
            local: alias.unwrap_or_else(|| last.clone()),
            imported: last.clone(),
            module: module.join("::"),
            kind: ImportKind::Named,
            type_only: false,
            start,
            end,
        });
    };
    let join =
        |extra: Vec<String>| -> Vec<String> { prefix.iter().cloned().chain(extra).collect() };
    match node.kind() {
        "identifier" | "crate" | "self" | "super" | "scoped_identifier" => {
            if let Some(segments) = path_segments(node, c.src) {
                emit(c, join(segments), None);
            }
        }
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .and_then(|p| path_segments(p, c.src));
            let alias = node
                .child_by_field_name("alias")
                .map(|a| c.text(a).to_owned());
            if let Some(segments) = path {
                emit(c, join(segments), alias);
            }
        }
        "scoped_use_list" => {
            let head = node
                .child_by_field_name("path")
                .and_then(|p| path_segments(p, c.src))
                .unwrap_or_default();
            let new_prefix = join(head);
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for child in list.named_children(&mut cursor) {
                    use_tree(child, &new_prefix, c, start, end);
                }
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                use_tree(child, prefix, c, start, end);
            }
        }
        "use_wildcard" => {
            let mut cursor = node.walk();
            let path = node
                .named_children(&mut cursor)
                .find_map(|child| path_segments(child, c.src))
                .unwrap_or_default();
            c.import(ImportFact {
                local: "*".into(),
                imported: "*".into(),
                module: join(path).join("::"),
                kind: ImportKind::Wildcard,
                type_only: false,
                start,
                end,
            });
        }
        _ => {}
    }
}

impl Rust {
    fn let_binding(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(pattern) = node.child_by_field_name("pattern") else {
            return;
        };
        let value = node.child_by_field_name("value");
        if pattern.kind() != "identifier" {
            let mut names = Vec::new();
            binding_names(pattern, c.src, &mut names);
            for name in names {
                c.declare(&name, false);
            }
            return;
        }
        let name = c.text(pattern).to_owned();
        let annotated = node
            .child_by_field_name("type")
            .and_then(|t| rust_type(c.text(t)));
        let literal = value
            .filter(|v| v.kind() == "struct_expression")
            .and_then(|v| v.child_by_field_name("name"))
            .and_then(|n| simple_type(c.text(n)));
        match literal {
            Some(ty) => c.declare_typed(&name, Some(ty), true, false),
            None => c.declare_typed(&name, annotated, false, false),
        }
        if let Some(value) = value.filter(|v| v.kind() == "closure_expression") {
            c.name_closure(value, &name);
        }
    }

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(mut function) = node.child_by_field_name("function") else {
            return;
        };
        if function.kind() == "generic_function" {
            if let Some(inner) = function.child_by_field_name("function") {
                function = inner;
            }
        }
        match function.kind() {
            "identifier" => c.call(CallSpec {
                name: c.text(function).to_owned(),
                kind: CallKind::Function,
                receiver: Receiver::None,
                range: (function.start_byte(), function.end_byte()),
            }),
            "scoped_identifier" => {
                let (Some(path), Some(name)) = (
                    function.child_by_field_name("path"),
                    function.child_by_field_name("name"),
                ) else {
                    return dynamic_call(c, function);
                };
                let receiver = match path_segments(path, c.src) {
                    Some(segments) => Receiver::Path(segments),
                    None => Receiver::Expr,
                };
                c.call(CallSpec {
                    name: c.text(name).to_owned(),
                    kind: CallKind::Static,
                    receiver,
                    range: (name.start_byte(), name.end_byte()),
                });
            }
            "field_expression" => {
                let (Some(value), Some(field)) = (
                    function.child_by_field_name("value"),
                    function.child_by_field_name("field"),
                ) else {
                    return dynamic_call(c, function);
                };
                c.call(CallSpec {
                    name: c.text(field).to_owned(),
                    kind: CallKind::Method,
                    receiver: receiver_from(value, c.src),
                    range: (field.start_byte(), field.end_byte()),
                });
            }
            _ => dynamic_call(c, function),
        }
    }
}

// ----- resolution -----

pub(crate) struct RustScheme;

enum Res {
    Module { path: String, ns: String },
    Unknown,
    External(String),
}

fn is_anchor(segment: &str) -> bool {
    matches!(segment, "crate" | "self" | "super")
}

fn crate_root(project: &ProjectIndex<'_>, from: &str) -> Option<String> {
    let mut dir = dirname(from).to_owned();
    loop {
        let join = |name: &str| {
            if dir.is_empty() {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            }
        };
        let (lib, main) = (join("lib.rs"), join("main.rs"));
        match (project.contains(&lib), project.contains(&main)) {
            (true, true) => return Some(if from == main { main } else { lib }),
            (true, false) => return Some(lib),
            (false, true) => return Some(main),
            _ => {}
        }
        if dir.is_empty() {
            return None;
        }
        dir = dirname(&dir).to_owned();
    }
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".rs").unwrap_or(name)
}

fn child_module(
    project: &ProjectIndex<'_>,
    file_path: &str,
    ns: &str,
    segment: &str,
) -> Option<(String, String)> {
    let file = project.file(file_path)?;
    let target = if ns.is_empty() {
        segment.to_owned()
    } else {
        format!("{ns}::{segment}")
    };
    let nested = format!("{target}::");
    let inline = file
        .callables
        .iter()
        .any(|c| c.namespace == target || c.namespace.starts_with(&nested))
        || file
            .types
            .iter()
            .any(|t| t.namespace == target || t.namespace.starts_with(&nested));
    if inline {
        return Some((file_path.to_owned(), target));
    }
    if !ns.is_empty() {
        return None;
    }
    let dir = dirname(file_path);
    let stem = file_stem(file_path);
    let base = if matches!(stem, "main" | "lib" | "mod") {
        dir.to_owned()
    } else {
        join_normalized(dir, stem)?
    };
    for candidate in [format!("{segment}.rs"), format!("{segment}/mod.rs")] {
        let path = join_normalized(&base, &candidate)?;
        if project.contains(&path) {
            return Some((path, String::new()));
        }
    }
    None
}

fn parent_module(
    project: &ProjectIndex<'_>,
    file_path: &str,
    ns: &str,
) -> Option<(String, String)> {
    if let Some((parent, _)) = ns.rsplit_once("::") {
        return Some((file_path.to_owned(), parent.to_owned()));
    }
    if !ns.is_empty() {
        return Some((file_path.to_owned(), String::new()));
    }
    let stem = file_stem(file_path);
    if matches!(stem, "main" | "lib") {
        return None;
    }
    let pdir = if stem == "mod" {
        dirname(dirname(file_path)).to_owned()
    } else {
        dirname(file_path).to_owned()
    };
    let base = pdir.rsplit('/').next().unwrap_or(&pdir).to_owned();
    let candidates = [
        join_normalized(&pdir, "mod.rs")?,
        join_normalized(&pdir, "main.rs")?,
        join_normalized(&pdir, "lib.rs")?,
        join_normalized(dirname(&pdir), &format!("{base}.rs"))?,
    ];
    candidates
        .into_iter()
        .find(|path| project.contains(path))
        .map(|path| (path, String::new()))
}

fn resolve_segments(
    project: &ProjectIndex<'_>,
    file_path: &str,
    file: &ExtractedFile,
    ns: &str,
    segments: &[String],
    depth: usize,
) -> Res {
    let Some(first) = segments.first() else {
        return Res::Module {
            path: file_path.to_owned(),
            ns: ns.to_owned(),
        };
    };
    let mut state: (String, String);
    let mut rest = &segments[1..];
    match first.as_str() {
        "crate" => match crate_root(project, file_path) {
            Some(root) => state = (root, String::new()),
            None => return Res::Unknown,
        },
        "self" => state = (file_path.to_owned(), ns.to_owned()),
        "super" => match parent_module(project, file_path, ns) {
            Some(parent) => state = parent,
            None => return Res::Unknown,
        },
        other => {
            let import = file.imports.iter().find(|i| {
                matches!(i.kind, ImportKind::Named | ImportKind::Namespace) && i.local == other
            });
            if let (Some(import), true) = (import, depth < 4) {
                let mut full: Vec<String> = import
                    .module
                    .split("::")
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect();
                if import.kind == ImportKind::Named {
                    full.push(import.imported.clone());
                }
                full.extend(segments[1..].iter().cloned());
                return match resolve_segments(project, file_path, file, ns, &full, depth + 1) {
                    Res::Unknown if full.first().is_some_and(|f| !is_anchor(f)) => {
                        Res::External(full.join("::"))
                    }
                    other => other,
                };
            }
            match child_module(project, file_path, ns, other) {
                Some(found) => state = found,
                None => return Res::Unknown,
            }
        }
    }
    while let Some((segment, tail)) = rest.split_first() {
        match segment.as_str() {
            "self" => {}
            "super" => match parent_module(project, &state.0, &state.1) {
                Some(parent) => state = parent,
                None => return Res::Unknown,
            },
            other => match child_module(project, &state.0, &state.1, other) {
                Some(found) => state = found,
                None => return Res::Unknown,
            },
        }
        rest = tail;
    }
    Res::Module {
        path: state.0,
        ns: state.1,
    }
}

/// Finds where `name` is defined starting from a module, following `use` re-exports.
fn find_member(
    project: &ProjectIndex<'_>,
    path: &str,
    ns: &str,
    name: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) {
    if depth > 5 {
        return;
    }
    let Some(file) = project.file(path) else {
        return;
    };
    let defines = file.callables.iter().any(|c| {
        c.name == name
            && c.parent.is_none()
            && c.container.is_none()
            && c.namespace == ns
            && matches!(c.kind, CallableKind::Function | CallableKind::Closure)
    }) || file
        .types
        .iter()
        .any(|t| t.name == name && t.namespace == ns);
    if defines {
        out.push((path.to_owned(), ns.to_owned()));
        return;
    }
    for import in file
        .imports
        .iter()
        .filter(|i| i.kind == ImportKind::Named && i.local == name)
    {
        let segments: Vec<String> = import
            .module
            .split("::")
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        if let Res::Module {
            path: next,
            ns: next_ns,
        } = resolve_segments(project, path, file, ns, &segments, 0)
        {
            find_member(project, &next, &next_ns, &import.imported, depth + 1, out);
        }
    }
}

impl Scheme for RustScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Rust
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Named && i.local == name)
        {
            let span = project_span(ctx.path, import.start, import.end);
            let segments: Vec<String> = import
                .module
                .split("::")
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect();
            match resolve_segments(ctx.project, ctx.path, ctx.file, ctx.namespace, &segments, 0) {
                Res::Module { path, ns } => {
                    let mut found = Vec::new();
                    find_member(ctx.project, &path, &ns, &import.imported, 0, &mut found);
                    for (target, target_ns) in found {
                        lookup.hits.push(ImportHit {
                            path: target,
                            member: import.imported.clone(),
                            via_type: None,
                            namespace: Some(target_ns),
                            rule: "static-import",
                            span: span.clone(),
                        });
                    }
                }
                Res::External(path) => lookup.external = Some((path, span)),
                Res::Unknown => {
                    if segments.first().is_some_and(|first| !is_anchor(first))
                        || segments.is_empty()
                    {
                        let full = if import.module.is_empty() {
                            import.imported.clone()
                        } else {
                            format!("{}::{}", import.module, import.imported)
                        };
                        lookup.external = Some((full, span));
                    }
                }
            }
        }
        lookup
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let Some(first) = path.first() else {
            return lookup;
        };
        let span = ctx
            .file
            .imports
            .iter()
            .find(|i| &i.local == first)
            .map(|i| project_span(ctx.path, i.start, i.end));
        match resolve_segments(ctx.project, ctx.path, ctx.file, ctx.namespace, path, 0) {
            Res::Module { path: found, ns } => lookup.hits.push(ModuleHit {
                path: found,
                namespace: Some(ns),
                rule: "module-path",
                span: span.unwrap_or_else(|| project_span(ctx.path, 0, 0)),
            }),
            Res::External(external) => {
                lookup.external = Some((
                    external,
                    span.unwrap_or_else(|| project_span(ctx.path, 0, 0)),
                ));
            }
            Res::Unknown => {}
        }
        lookup
    }
}
