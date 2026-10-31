//! C++: local `#include` graph with header/source companions, namespaces, classes with inline and
//! out-of-line members, constructors, lambdas and concrete receivers. Nothing is preprocessed or
//! executed; templates, virtual dispatch beyond declared bases, and ADL stay uncertain.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, BareTypeCall, ImportHit, ImportLookup, ModuleHit,
    ModuleLookup, Peer, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Cpp;

impl Syntax for Cpp {
    fn language(&self) -> LanguageId {
        LanguageId::Cpp
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves local `#include \"..\"` headers and same-stem companion sources, namespaces, `using` declarations, qualified calls, inline and out-of-line members, constructors, and `this->` calls".into(),
            "headers are matched to definitions only through the include graph; declarations without a definition in the workspace stay unresolved".into(),
            "the preprocessor is not executed: macro-generated code is invisible, and both branches of conditional blocks are analysed (duplicate definitions become possible targets)".into(),
            "templates, ADL, virtual dispatch through untyped pointers, operator overloads, function pointers and implicit default construction stay unresolved".into(),
            "out-of-line members qualified by a namespace-like name are treated as members of a type with that name".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "preproc_include" => {
                if let Some(path) = node.child_by_field_name("path") {
                    if path.kind() == "string_literal" {
                        c.import(ImportFact {
                            local: "*".into(),
                            imported: "*".into(),
                            module: unquote(c.text(path)).to_owned(),
                            kind: ImportKind::Include,
                            type_only: false,
                            start: node.start_byte() as u32,
                            end: node.end_byte() as u32,
                        });
                    }
                }
                Action::Skip
            }
            "using_declaration" => {
                self.using(node, c);
                Action::Skip
            }
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| c.text(n).to_owned());
                match name {
                    Some(name) if c.namespace().is_empty() => Action::Namespace(name),
                    Some(name) => Action::Namespace(format!("{}::{name}", c.namespace())),
                    None => Action::Descend,
                }
            }
            "class_specifier" | "struct_specifier" => {
                let (Some(name), Some(_)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    return Action::Skip;
                };
                let mut bases = Vec::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "base_class_clause" {
                        let mut inner = child.walk();
                        for base in child.named_children(&mut inner) {
                            if let Some(segments) = path_segments(base, c.src) {
                                bases.extend(segments.last().cloned());
                            }
                        }
                    }
                }
                let text = c.text(name);
                let simple = last_segment(text).to_owned();
                Action::Type(TypeSpec {
                    name: simple,
                    name_range: (name.start_byte(), name.end_byte()),
                    kind: if node.kind() == "struct_specifier" {
                        TypeKind::Struct
                    } else {
                        TypeKind::Class
                    },
                    bases,
                })
            }
            "function_definition" => self.function(node, c),
            "lambda_expression" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: node
                    .child_by_field_name("declarator")
                    .map(|d| params(d, c))
                    .unwrap_or_default(),
            }),
            "declaration" => {
                self.declaration(node, c);
                Action::Descend
            }
            "call_expression" => {
                self.call(node, c);
                Action::Descend
            }
            "new_expression" => {
                self.creation(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn function_declarator(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.child_by_field_name("declarator")?;
    loop {
        match current.kind() {
            "function_declarator" => return Some(current),
            "pointer_declarator"
            | "reference_declarator"
            | "parenthesized_declarator"
            | "array_declarator" => {
                current = current
                    .child_by_field_name("declarator")
                    .or_else(|| current.named_child(0))?;
            }
            _ => return None,
        }
    }
}

fn declared_identifier<'t>(node: Node<'t>) -> Option<Node<'t>> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node),
        "init_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "parenthesized_declarator"
        | "array_declarator"
        | "function_declarator" => node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
            .and_then(declared_identifier),
        _ => None,
    }
}

fn flatten(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "qualified_identifier" => {
            if let Some(scope) = node.child_by_field_name("scope") {
                flatten(scope, src, out);
            }
            if let Some(name) = node.child_by_field_name("name") {
                flatten(name, src, out);
            }
        }
        _ => out.push(src[node.byte_range()].to_owned()),
    }
}

fn params(declarator: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(list) = declarator.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor) {
        if parameter.kind() != "parameter_declaration" {
            continue;
        }
        let Some(name) = parameter
            .child_by_field_name("declarator")
            .and_then(declared_identifier)
        else {
            continue;
        };
        let ty = parameter
            .child_by_field_name("type")
            .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
            .and_then(|t| simple_type(c.text(t)));
        out.push((c.text(name).to_owned(), ty));
    }
    out
}

impl Cpp {
    fn function(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        let Some(declarator) = function_declarator(node) else {
            return Action::Descend;
        };
        let Some(name_node) = declarator.child_by_field_name("declarator") else {
            return Action::Descend;
        };
        let mut segments = Vec::new();
        flatten(name_node, c.src, &mut segments);
        let Some((name, scope)) = segments.split_last() else {
            return Action::Descend;
        };
        let name = name.trim_start_matches("template ").to_owned();
        let (container, namespace) = match scope.split_last() {
            Some((container, namespaces)) => {
                let mut ns = c.namespace().to_owned();
                for part in namespaces {
                    if !ns.is_empty() {
                        ns.push_str("::");
                    }
                    ns.push_str(part);
                }
                (Some(container.clone()), Some(ns))
            }
            None => (None, None),
        };
        let owner = container
            .clone()
            .or_else(|| c.current_type().map(str::to_owned));
        let in_type = owner.is_some() && !c.in_callable();
        let kind = if !in_type {
            CallableKind::Function
        } else if owner.as_deref() == Some(name.as_str()) {
            CallableKind::Constructor
        } else {
            CallableKind::Method
        };
        let range = if name_node.kind() == "qualified_identifier" {
            let mut inner = name_node;
            while let Some(next) = inner.child_by_field_name("name") {
                if next.kind() == "qualified_identifier" {
                    inner = next;
                } else {
                    inner = next;
                    break;
                }
            }
            (inner.start_byte(), inner.end_byte())
        } else {
            (name_node.start_byte(), name_node.end_byte())
        };
        Action::Callable(CallableSpec {
            name: Some(name),
            name_range: Some(range),
            kind,
            container,
            namespace,
            params: params(declarator, c),
        })
    }

    fn using(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let text = c.text(node).trim().trim_end_matches(';').trim().to_owned();
        let rest = text.strip_prefix("using").unwrap_or(&text).trim();
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        if let Some(namespace) = rest.strip_prefix("namespace ") {
            c.import(ImportFact {
                local: "*".into(),
                imported: "*".into(),
                module: namespace.trim().trim_start_matches("::").to_owned(),
                kind: ImportKind::Wildcard,
                type_only: false,
                start,
                end,
            });
            return;
        }
        let path = rest.trim_start_matches("::");
        if let Some((module, imported)) = path.rsplit_once("::") {
            c.import(ImportFact {
                local: imported.to_owned(),
                imported: imported.to_owned(),
                module: module.to_owned(),
                kind: ImportKind::Named,
                type_only: false,
                start,
                end,
            });
        }
    }

    fn declaration(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let ty_node = node.child_by_field_name("type");
        let class_type = ty_node
            .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
            .and_then(|t| simple_type(c.text(t)));
        let mut cursor = node.walk();
        let declarators: Vec<_> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for declarator in declarators {
            let Some(name) = declared_identifier(declarator) else {
                continue;
            };
            let text = c.text(name).to_owned();
            let by_value = matches!(declarator.kind(), "identifier" | "init_declarator");
            let value = if declarator.kind() == "init_declarator" {
                declarator.child_by_field_name("value")
            } else {
                None
            };
            let constructed = value
                .filter(|v| v.kind() == "new_expression")
                .and_then(|v| v.child_by_field_name("type"))
                .and_then(|t| simple_type(c.text(t)));
            match (&constructed, &class_type) {
                (Some(ty), _) => c.declare_typed(&text, Some(ty.clone()), true, false),
                (None, Some(ty)) => c.declare_typed(&text, Some(ty.clone()), by_value, false),
                (None, None) => c.declare_typed(&text, None, false, false),
            }
            if let Some(value) = value {
                if value.kind() == "lambda_expression" {
                    c.name_closure(value, &text);
                }
                // `Foo y(1);` constructs a Foo.
                if value.kind() == "argument_list" {
                    if let Some(ty) = ty_node
                        .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
                    {
                        let mut segments = Vec::new();
                        flatten(ty, c.src, &mut segments);
                        if let Some((last, prefix)) = segments.split_last() {
                            let leaf = if ty.kind() == "qualified_identifier" {
                                ty.child_by_field_name("name").unwrap_or(ty)
                            } else {
                                ty
                            };
                            c.call(CallSpec {
                                name: last.clone(),
                                kind: CallKind::Constructor,
                                receiver: if prefix.is_empty() {
                                    Receiver::None
                                } else {
                                    Receiver::Path(prefix.to_vec())
                                },
                                range: (leaf.start_byte(), leaf.end_byte()),
                            });
                        }
                    }
                }
            }
        }
    }

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(mut function) = node.child_by_field_name("function") else {
            return;
        };
        if function.kind() == "template_function" {
            if let Some(inner) = function.child_by_field_name("name") {
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
            "field_expression" => {
                let (Some(argument), Some(field)) = (
                    function.child_by_field_name("argument"),
                    function.child_by_field_name("field"),
                ) else {
                    return dynamic_call(c, function);
                };
                c.call(CallSpec {
                    name: c.text(field).to_owned(),
                    kind: CallKind::Method,
                    receiver: receiver_from(argument, c.src),
                    range: (field.start_byte(), field.end_byte()),
                });
            }
            "qualified_identifier" => {
                let mut segments = Vec::new();
                flatten(function, c.src, &mut segments);
                let mut leaf = function;
                while let Some(next) = leaf.child_by_field_name("name") {
                    leaf = next;
                    if next.kind() != "qualified_identifier" {
                        break;
                    }
                }
                let Some((name, prefix)) = segments.split_last() else {
                    return;
                };
                c.call(CallSpec {
                    name: name.clone(),
                    kind: if prefix.is_empty() {
                        CallKind::Function
                    } else {
                        CallKind::Static
                    },
                    receiver: if prefix.is_empty() {
                        Receiver::None
                    } else {
                        Receiver::Path(prefix.to_vec())
                    },
                    range: (leaf.start_byte(), leaf.end_byte()),
                });
            }
            _ => dynamic_call(c, function),
        }
    }

    fn creation(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        if !matches!(ty.kind(), "type_identifier" | "qualified_identifier") {
            return dynamic_call(c, ty);
        }
        let mut segments = Vec::new();
        flatten(ty, c.src, &mut segments);
        let Some((name, prefix)) = segments.split_last() else {
            return;
        };
        let leaf = if ty.kind() == "qualified_identifier" {
            ty.child_by_field_name("name").unwrap_or(ty)
        } else {
            ty
        };
        c.call(CallSpec {
            name: name.clone(),
            kind: CallKind::Constructor,
            receiver: if prefix.is_empty() {
                Receiver::None
            } else {
                Receiver::Path(prefix.to_vec())
            },
            range: (leaf.start_byte(), leaf.end_byte()),
        });
    }
}

// ----- resolution -----

pub(crate) struct CppScheme;

const SOURCE_EXTENSIONS: [&str; 8] = ["cpp", "cc", "cxx", "c", "h", "hh", "hpp", "hxx"];

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn companions(ctx: &ResolutionContext<'_>, path: &str) -> Vec<String> {
    let wanted = stem(path);
    let dir = dirname(path);
    let all: Vec<&str> = ctx
        .project
        .files()
        .filter(|(candidate, file)| {
            *candidate != path
                && file.language == LanguageId::Cpp
                && stem(candidate) == wanted
                && candidate
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| SOURCE_EXTENSIONS.contains(&ext))
        })
        .map(|(candidate, _)| candidate)
        .collect();
    let same_dir: Vec<String> = all
        .iter()
        .filter(|candidate| dirname(candidate) == dir)
        .map(|candidate| (*candidate).to_owned())
        .collect();
    if !same_dir.is_empty() {
        return same_dir;
    }
    // A unique same-stem file elsewhere (include/ vs src/) is accepted as the companion.
    match all.as_slice() {
        [only] => vec![(*only).to_owned()],
        _ => Vec::new(),
    }
}

fn resolve_include(ctx: &ResolutionContext<'_>, from: &str, module: &str) -> Option<String> {
    for candidate in [
        join_normalized(dirname(from), module),
        join_normalized("", module),
    ]
    .into_iter()
    .flatten()
    {
        if ctx.project.contains(&candidate) {
            return Some(candidate);
        }
    }
    let suffix = format!("/{}", module.trim_start_matches("./"));
    let matches: Vec<&str> = ctx
        .project
        .paths()
        .filter(|path| path.ends_with(&suffix))
        .collect();
    match matches.as_slice() {
        [only] => Some((*only).to_owned()),
        _ => None,
    }
}

/// Files reachable from the current one through local includes and companions.
fn closure(ctx: &ResolutionContext<'_>) -> Vec<(String, &'static str)> {
    let mut seen = vec![ctx.path.to_owned()];
    let mut out = Vec::new();
    let mut frontier = vec![ctx.path.to_owned()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for path in &frontier {
            let Some(file) = ctx.project.file(path) else {
                continue;
            };
            let mut found: Vec<(String, &'static str)> = Vec::new();
            for import in file
                .imports
                .iter()
                .filter(|i| i.kind == ImportKind::Include)
            {
                if let Some(target) = resolve_include(ctx, path, &import.module) {
                    found.push((target, "include"));
                }
            }
            for companion in companions(ctx, path) {
                found.push((companion, "companion-file"));
            }
            for (target, rule) in found {
                if !seen.contains(&target) {
                    seen.push(target.clone());
                    out.push((target.clone(), rule));
                    next.push(target);
                }
            }
        }
        frontier = next;
    }
    out
}

fn namespace_chain(namespace: &str) -> Vec<String> {
    let mut chain = vec![String::new()];
    let mut built = String::new();
    for part in namespace.split("::").filter(|part| !part.is_empty()) {
        if !built.is_empty() {
            built.push_str("::");
        }
        built.push_str(part);
        chain.push(built.clone());
    }
    chain
}

impl Scheme for CppScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Cpp
    }

    fn bare_call_sees_enclosing_type(&self) -> bool {
        true
    }

    fn bare_type_call(&self) -> BareTypeCall {
        BareTypeCall::Construct
    }

    fn related_files(&self, ctx: &ResolutionContext<'_>, path: &str) -> Vec<String> {
        let mut files = vec![path.to_owned()];
        files.extend(companions(ctx, path));
        files
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut namespaces = namespace_chain(ctx.namespace);
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
        {
            namespaces.push(import.module.clone());
        }
        let mut peers = Vec::new();
        let mut files = closure(ctx);
        files.push((ctx.path.to_owned(), "same-file"));
        for (path, rule) in files {
            for namespace in &namespaces {
                if path == ctx.path && namespace_chain(ctx.namespace).contains(namespace) {
                    continue;
                }
                peers.push(Peer {
                    path: path.clone(),
                    namespace: Some(namespace.clone()),
                    functions: true,
                    rule,
                    span: None,
                });
            }
        }
        peers
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let joined = path.join("::");
        let mut candidates = vec![joined.clone()];
        for prefix in namespace_chain(ctx.namespace).into_iter().rev() {
            if !prefix.is_empty() {
                candidates.push(format!("{prefix}::{joined}"));
            }
        }
        let mut files = closure(ctx);
        files.push((ctx.path.to_owned(), "same-file"));
        for candidate in candidates {
            let mut found = false;
            for (file, _) in &files {
                let Some(facts) = ctx.project.file(file) else {
                    continue;
                };
                let declares = facts
                    .callables
                    .iter()
                    .any(|c| c.namespace == candidate && c.kind != CallableKind::Synthetic)
                    || facts.types.iter().any(|t| t.namespace == candidate);
                if declares {
                    found = true;
                    lookup.hits.push(ModuleHit {
                        path: file.clone(),
                        namespace: Some(candidate.clone()),
                        rule: "namespace-qualified",
                        span: project_span(ctx.path, 0, 0),
                    });
                }
            }
            if found {
                break;
            }
        }
        lookup
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        let mut files = closure(ctx);
        files.push((ctx.path.to_owned(), "same-file"));
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Named && i.local == name)
        {
            let span = project_span(ctx.path, import.start, import.end);
            for (file, _) in &files {
                lookup.hits.push(ImportHit {
                    path: file.clone(),
                    member: import.imported.clone(),
                    via_type: None,
                    namespace: Some(import.module.clone()),
                    rule: "using-declaration",
                    span: span.clone(),
                });
            }
        }
        lookup
    }
}
