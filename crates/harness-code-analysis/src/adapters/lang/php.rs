//! PHP: namespaces, `use`/`use function` imports, classes with `__construct`, `$this`/`self`/
//! `parent` calls and literal `include`/`require`. Variable callees keep their `$` so they never
//! collide with function names.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, ImportHit, ImportLookup, ModuleHit, ModuleLookup, Peer,
    ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Php;

const TYPE_KINDS: [(&str, TypeKind); 4] = [
    ("class_declaration", TypeKind::Class),
    ("interface_declaration", TypeKind::Interface),
    ("trait_declaration", TypeKind::Trait),
    ("enum_declaration", TypeKind::Enum),
];

impl Syntax for Php {
    fn language(&self) -> LanguageId {
        LanguageId::Php
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_php::LANGUAGE_PHP_ONLY.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves namespaces, `use`/`use function` imports and aliases, constructors, static calls, `$this`/`self`/`static`/`parent` calls, and receivers typed by `new`".into(),
            "functions in other files are visible only through literal `include`/`require` (dynamic includes and autoloaders are not followed)".into(),
            "variable functions (`$f()`), variable method names, `call_user_func`, magic methods, and traits' use-conflict rules stay unresolved".into(),
            "classes are found by namespace declaration, not by PSR-4 file layout".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        let kind = node.kind();
        if let Some((_, type_kind)) = TYPE_KINDS.iter().find(|(name, _)| *name == kind) {
            let Some(name) = node.child_by_field_name("name") else {
                return Action::Descend;
            };
            let mut bases = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "base_clause" | "class_interface_clause") {
                    collect_names(child, c.src, &mut bases);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for member in body.named_children(&mut cursor) {
                    if member.kind() == "use_declaration" {
                        collect_names(member, c.src, &mut bases);
                    }
                }
            }
            return Action::Type(TypeSpec {
                name: c.text(name).to_owned(),
                name_range: (name.start_byte(), name.end_byte()),
                kind: *type_kind,
                bases,
            });
        }
        match kind {
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| c.text(n).to_owned())
                    .unwrap_or_default();
                if node.child_by_field_name("body").is_some() {
                    Action::Namespace(name)
                } else {
                    c.set_package(name);
                    Action::Skip
                }
            }
            "namespace_use_declaration" => {
                self.use_declaration(node, c);
                Action::Skip
            }
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                self.include(node, c);
                Action::Descend
            }
            "function_definition" | "method_declaration" => {
                let name = node.child_by_field_name("name");
                let text = name.map(|n| c.text(n).to_owned());
                let callable_kind = match (kind, text.as_deref()) {
                    ("method_declaration", Some("__construct")) => CallableKind::Constructor,
                    ("method_declaration", _) => CallableKind::Method,
                    _ => CallableKind::Function,
                };
                Action::Callable(CallableSpec {
                    name: text,
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: callable_kind,
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            "anonymous_function" | "arrow_function" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: params(node, c),
            }),
            "assignment_expression" => {
                self.assignment(node, c);
                Action::Descend
            }
            "function_call_expression" => {
                self.function_call(node, c);
                Action::Descend
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                self.member_call(node, c);
                Action::Descend
            }
            "scoped_call_expression" => {
                self.scoped_call(node, c);
                Action::Descend
            }
            "object_creation_expression" => {
                self.creation(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn collect_names(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "name" => out.push(src[node.byte_range()].to_owned()),
        "qualified_name" => {
            if let Some(name) = qualified_last(node) {
                out.push(src[name.byte_range()].to_owned());
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_names(child, src, out);
            }
        }
    }
}

fn params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let Some(name) = parameter.child_by_field_name("name") else {
                continue;
            };
            let ty = parameter
                .child_by_field_name("type")
                .and_then(|t| simple_type(c.text(t).trim_start_matches('?')));
            out.push((c.text(name).to_owned(), ty));
        }
    }
    // `function () use ($x)` captures.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "anonymous_function_use_clause" {
            let mut inner = child.walk();
            for variable in child.named_children(&mut inner) {
                if variable.kind() == "variable_name" {
                    out.push((c.text(variable).to_owned(), None));
                }
            }
        }
    }
    out
}

/// The unlabeled final `name` of a `qualified_name`.
fn qualified_last(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.named_child(node.named_child_count().saturating_sub(1)))
}

fn type_name(node: Node<'_>, src: &str) -> Option<String> {
    path_segments(node, src).and_then(|segments| segments.last().cloned())
}

/// Receiver of `$x->m()`: `$this` is self, other variables keep their `$` to match locals.
fn php_receiver(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "variable_name" => {
            let text = &src[node.byte_range()];
            if text == "$this" {
                Receiver::SelfValue
            } else {
                Receiver::Path(vec![text.to_owned()])
            }
        }
        _ => Receiver::Expr,
    }
}

impl Php {
    fn use_declaration(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let text = c.text(node).trim_start();
        let is_function = text.starts_with("use function");
        if text.starts_with("use const") {
            return;
        }
        let mut cursor = node.walk();
        let group_prefix = node
            .children(&mut cursor)
            .find(|child| child.kind() == "namespace_name")
            .and_then(|n| path_segments(n, c.src))
            .unwrap_or_default();
        let mut clauses = Vec::new();
        collect_clauses(node, &mut clauses);
        for clause in clauses {
            let mut inner = clause.walk();
            let target = clause
                .named_children(&mut inner)
                .find(|n| matches!(n.kind(), "qualified_name" | "name" | "namespace_name"));
            let Some(target) = target else { continue };
            let mut segments = group_prefix.clone();
            segments.extend(path_segments(target, c.src).unwrap_or_default());
            let Some((imported, module)) = segments.split_last() else {
                continue;
            };
            let local = clause
                .child_by_field_name("alias")
                .map_or_else(|| imported.clone(), |a| c.text(a).to_owned());
            c.import(ImportFact {
                local,
                imported: imported.clone(),
                module: module.join("\\"),
                kind: if is_function {
                    ImportKind::Function
                } else {
                    ImportKind::Named
                },
                type_only: false,
                start,
                end,
            });
        }
    }

    fn include(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let text = c.text(node).to_owned();
        let mut stack = vec![node];
        let mut literal = None;
        while let Some(current) = stack.pop() {
            if matches!(current.kind(), "string" | "encapsed_string") {
                literal = Some(unquote(c.text(current)).to_owned());
                break;
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
        let Some(path) = literal else { return };
        let relative = if text.contains("__DIR__") {
            path.trim_start_matches('/').to_owned()
        } else {
            path
        };
        c.import(ImportFact {
            local: "*".into(),
            imported: "*".into(),
            module: relative,
            kind: ImportKind::Include,
            type_only: false,
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
        });
    }

    fn assignment(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        if left.kind() != "variable_name" {
            return;
        }
        let name = c.text(left).to_owned();
        let right = node.child_by_field_name("right");
        let created = right
            .filter(|r| r.kind() == "object_creation_expression")
            .and_then(creation_type)
            .and_then(|ty| type_name(ty, c.src));
        match created {
            Some(ty) => c.declare_typed(&name, Some(ty), true, false),
            None => c.declare_typed(&name, None, false, false),
        }
        if let Some(right) = right {
            if matches!(right.kind(), "anonymous_function" | "arrow_function") {
                c.name_closure(right, &name);
            }
        }
    }

    fn function_call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        match function.kind() {
            "name" => c.call(CallSpec {
                name: c.text(function).to_owned(),
                kind: CallKind::Function,
                receiver: Receiver::None,
                range: (function.start_byte(), function.end_byte()),
            }),
            "qualified_name" => {
                let Some(name) = qualified_last(function) else {
                    return dynamic_call(c, function);
                };
                let segments = path_segments(function, c.src).unwrap_or_default();
                let prefix = segments[..segments.len().saturating_sub(1)].to_vec();
                c.call(CallSpec {
                    name: c.text(name).to_owned(),
                    kind: CallKind::Function,
                    receiver: if prefix.is_empty() {
                        Receiver::None
                    } else {
                        Receiver::Path(prefix)
                    },
                    range: (name.start_byte(), name.end_byte()),
                });
            }
            "variable_name" => c.call(CallSpec {
                name: c.text(function).to_owned(),
                kind: CallKind::Function,
                receiver: Receiver::None,
                range: (function.start_byte(), function.end_byte()),
            }),
            _ => dynamic_call(c, function),
        }
    }

    fn member_call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (Some(object), Some(name)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("name"),
        ) else {
            return;
        };
        if name.kind() != "name" {
            return dynamic_call(c, name);
        }
        c.call(CallSpec {
            name: c.text(name).to_owned(),
            kind: CallKind::Method,
            receiver: php_receiver(object, c.src),
            range: (name.start_byte(), name.end_byte()),
        });
    }

    fn scoped_call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (Some(scope), Some(name)) = (
            node.child_by_field_name("scope"),
            node.child_by_field_name("name"),
        ) else {
            return;
        };
        if name.kind() != "name" {
            return dynamic_call(c, name);
        }
        let receiver = match scope.kind() {
            "relative_scope" => match c.text(scope) {
                "parent" => Receiver::Super,
                _ => Receiver::SelfValue,
            },
            "name" | "qualified_name" => {
                Receiver::Path(path_segments(scope, c.src).unwrap_or_default())
            }
            _ => Receiver::Expr,
        };
        c.call(CallSpec {
            name: c.text(name).to_owned(),
            kind: CallKind::Static,
            receiver,
            range: (name.start_byte(), name.end_byte()),
        });
    }

    fn creation(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(ty) = creation_type(node) else {
            return;
        };
        match ty.kind() {
            "name" => c.call(CallSpec {
                name: c.text(ty).to_owned(),
                kind: CallKind::Constructor,
                receiver: Receiver::None,
                range: (ty.start_byte(), ty.end_byte()),
            }),
            "qualified_name" => {
                let (Some(name), Some(segments)) = (qualified_last(ty), path_segments(ty, c.src))
                else {
                    return;
                };
                let prefix = segments[..segments.len() - 1].to_vec();
                c.call(CallSpec {
                    name: c.text(name).to_owned(),
                    kind: CallKind::Constructor,
                    receiver: if prefix.is_empty() {
                        Receiver::None
                    } else {
                        Receiver::Path(prefix)
                    },
                    range: (name.start_byte(), name.end_byte()),
                });
            }
            _ => dynamic_call(c, ty),
        }
    }
}

fn collect_clauses<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "namespace_use_clause" {
            out.push(child);
        } else {
            collect_clauses(child, out);
        }
    }
}

fn creation_type(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|n| {
        matches!(
            n.kind(),
            "name" | "qualified_name" | "relative_scope" | "variable_name"
        )
    });
    found
}

// ----- resolution -----

pub(crate) struct PhpScheme;

fn declares_in<'a>(
    ctx: &'a ResolutionContext<'_>,
    namespace: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    ctx.project
        .files()
        .filter(move |(_, file)| {
            file.language == LanguageId::Php
                && (file.types.iter().any(|t| t.namespace == namespace)
                    || file
                        .callables
                        .iter()
                        .any(|c| c.namespace == namespace && c.kind != CallableKind::Synthetic))
        })
        .map(|(path, _)| path)
}

impl Scheme for PhpScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Php
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        for import in ctx.file.imports.iter().filter(|i| {
            i.local == name && matches!(i.kind, ImportKind::Named | ImportKind::Function)
        }) {
            let span = project_span(ctx.path, import.start, import.end);
            let mut declared_namespace = false;
            for path in declares_in(ctx, &import.module) {
                declared_namespace = true;
                let file = ctx.project.file(path).expect("listed file exists");
                let found = match import.kind {
                    ImportKind::Function => file.callables.iter().any(|c| {
                        c.name == import.imported
                            && c.namespace == import.module
                            && c.container.is_none()
                            && c.parent.is_none()
                    }),
                    _ => file
                        .types
                        .iter()
                        .any(|t| t.name == import.imported && t.namespace == import.module),
                };
                if found {
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: import.imported.clone(),
                        via_type: None,
                        namespace: Some(import.module.clone()),
                        rule: "static-import",
                        span: span.clone(),
                    });
                }
            }
            if !declared_namespace {
                let full = if import.module.is_empty() {
                    import.imported.clone()
                } else {
                    format!("{}\\{}", import.module, import.imported)
                };
                lookup.external = Some((full, span));
            }
        }
        lookup
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let Some(first) = path.first() else {
            return lookup;
        };
        let joined = path.join("\\");
        let mut candidates = Vec::new();
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| &i.local == first && i.kind == ImportKind::Named)
        {
            let mut full = if import.module.is_empty() {
                import.imported.clone()
            } else {
                format!("{}\\{}", import.module, import.imported)
            };
            for rest in &path[1..] {
                full.push('\\');
                full.push_str(rest);
            }
            candidates.push((full, project_span(ctx.path, import.start, import.end)));
        }
        let relative = if ctx.namespace.is_empty() {
            joined.clone()
        } else {
            format!("{}\\{joined}", ctx.namespace)
        };
        let here = project_span(ctx.path, 0, 0);
        candidates.push((relative, here.clone()));
        candidates.push((joined, here));
        for (namespace, span) in candidates {
            let files: Vec<&str> = declares_in(ctx, &namespace).collect();
            if !files.is_empty() {
                for file in files {
                    lookup.hits.push(ModuleHit {
                        path: file.to_owned(),
                        namespace: Some(namespace.clone()),
                        rule: "namespace-alias",
                        span: span.clone(),
                    });
                }
                break;
            }
        }
        lookup
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut peers = Vec::new();
        for path in declares_in(ctx, ctx.namespace) {
            if path != ctx.path {
                peers.push(Peer {
                    path: path.to_owned(),
                    namespace: Some(ctx.namespace.to_owned()),
                    functions: false,
                    rule: "same-namespace",
                    span: None,
                });
            }
        }
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Include)
        {
            let relative = join_normalized(dirname(ctx.path), &import.module);
            let root = join_normalized("", &import.module);
            let target = [relative, root]
                .into_iter()
                .flatten()
                .find(|candidate| ctx.project.contains(candidate));
            if let Some(path) = target {
                peers.push(Peer {
                    path,
                    namespace: None,
                    functions: true,
                    rule: "literal-include",
                    span: Some(project_span(ctx.path, import.start, import.end)),
                });
            }
        }
        peers
    }
}
