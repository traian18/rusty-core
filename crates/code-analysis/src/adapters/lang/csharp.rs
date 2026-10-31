//! C#: namespaces (block and file-scoped), `using` directives and aliases, constructors,
//! local functions, lambdas and concrete receivers.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    project_span, ImportHit, ImportLookup, ModuleHit, ModuleLookup, Peer, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct CSharp;

const TYPE_KINDS: [(&str, TypeKind); 5] = [
    ("class_declaration", TypeKind::Class),
    ("struct_declaration", TypeKind::Struct),
    ("interface_declaration", TypeKind::Interface),
    ("enum_declaration", TypeKind::Enum),
    ("record_declaration", TypeKind::Class),
];

impl Syntax for CSharp {
    fn language(&self) -> LanguageId {
        LanguageId::CSharp
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves namespace-scoped types, `using` namespaces/aliases/static usings, constructors, static calls, `this`/`base` calls, and receivers typed by `new` or declarations".into(),
            "overloads and overriding subtypes are reported as possible targets, never as one exact target".into(),
            "delegates, reflection, `dynamic`, extension methods, partial-class merging, and target-typed `new()` stay unresolved".into(),
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
                if child.kind() == "base_list" {
                    collect_bases(child, c.src, &mut bases);
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
            "namespace_declaration" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| c.text(n).to_owned())
                    .unwrap_or_default();
                let full = if c.namespace().is_empty() {
                    name
                } else {
                    format!("{}.{name}", c.namespace())
                };
                Action::Namespace(full)
            }
            "file_scoped_namespace_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    c.set_package(c.text(name).to_owned());
                }
                Action::Skip
            }
            "using_directive" => {
                self.using(node, c);
                Action::Skip
            }
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                let name = node.child_by_field_name("name");
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: match kind {
                        "method_declaration" => CallableKind::Method,
                        "constructor_declaration" => CallableKind::Constructor,
                        _ => CallableKind::Function,
                    },
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            "accessor_declaration" => {
                let property = node
                    .parent()
                    .and_then(|list| list.parent())
                    .and_then(|prop| prop.child_by_field_name("name"))
                    .map(|n| c.text(n).to_owned())
                    .unwrap_or_default();
                let accessor = node
                    .child_by_field_name("name")
                    .map(|n| c.text(n).to_owned())
                    .unwrap_or_else(|| "get".into());
                Action::Callable(CallableSpec {
                    name: Some(format!("{accessor}_{property}")),
                    name_range: None,
                    kind: CallableKind::Method,
                    container: None,
                    namespace: None,
                    params: Vec::new(),
                })
            }
            "lambda_expression" | "anonymous_method_expression" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: lambda_params(node, c),
            }),
            "variable_declaration" => {
                self.locals(node, c);
                Action::Descend
            }
            "foreach_statement" | "catch_declaration" => {
                if let Some(name) = node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("name"))
                {
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| simple_type(c.text(t)))
                        .filter(|t| t != "var");
                    c.declare_typed(c.text(name), ty, false, false);
                }
                Action::Descend
            }
            "invocation_expression" => {
                self.invocation(node, c);
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

fn collect_bases(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => out.push(src[node.byte_range()].to_owned()),
        "qualified_name" => {
            if let Some(name) = node.child_by_field_name("name") {
                out.push(src[name.byte_range()].to_owned());
            }
        }
        "generic_name" => {
            if let Some(first) = node.named_child(0) {
                out.push(src[first.byte_range()].to_owned());
            }
        }
        "type_argument_list" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_bases(child, src, out);
            }
        }
    }
}

fn params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = parameters.walk();
    for child in parameters.named_children(&mut cursor) {
        if child.kind() == "parameter" {
            if let Some(name) = child.child_by_field_name("name") {
                let ty = child
                    .child_by_field_name("type")
                    .and_then(|t| simple_type(c.text(t)));
                out.push((c.text(name).to_owned(), ty));
            }
        }
    }
    out
}

fn lambda_params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    if parameters.kind() == "parameter_list" {
        return params(node, c);
    }
    let mut names = Vec::new();
    binding_names(parameters, c.src, &mut names);
    if names.is_empty() {
        names.push(c.text(parameters).to_owned());
    }
    names.into_iter().map(|n| (n, None)).collect()
}

impl CSharp {
    fn using(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let text = c.text(node);
        let is_static = text.contains("using static");
        let alias = node.child_by_field_name("name");
        let mut cursor = node.walk();
        let target = node
            .named_children(&mut cursor)
            .filter(|n| Some(n.id()) != alias.map(|a| a.id()))
            .find(|n| matches!(n.kind(), "identifier" | "qualified_name" | "generic_name"));
        let Some(target) = target else { return };
        let module = path_segments(target, c.src)
            .map(|s| s.join("."))
            .unwrap_or_else(|| c.text(target).to_owned());
        let (local, imported, kind) = match (alias, is_static) {
            (Some(alias), _) => (
                c.text(alias).to_owned(),
                "*".to_owned(),
                ImportKind::Namespace,
            ),
            (None, true) => ("*".into(), "*".into(), ImportKind::Static),
            (None, false) => ("*".into(), "*".into(), ImportKind::Wildcard),
        };
        c.import(ImportFact {
            local,
            imported,
            module,
            kind,
            type_only: false,
            start,
            end,
        });
    }

    fn locals(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let declared = node
            .child_by_field_name("type")
            .map(|t| c.text(t))
            .and_then(simple_type)
            .filter(|ty| ty != "var");
        let mut cursor = node.walk();
        for declarator in node.named_children(&mut cursor) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            let mut inner = declarator.walk();
            let created = declarator
                .named_children(&mut inner)
                .find(|n| n.kind() == "object_creation_expression")
                .and_then(|v| v.child_by_field_name("type"))
                .and_then(|t| simple_type(c.text(t)));
            match created {
                Some(ty) => c.declare_typed(c.text(name), Some(ty), true, false),
                None => c.declare_typed(c.text(name), declared.clone(), false, false),
            }
            let mut inner = declarator.walk();
            let lambda = declarator.named_children(&mut inner).find(|n| {
                matches!(
                    n.kind(),
                    "lambda_expression" | "anonymous_method_expression"
                )
            });
            if let Some(lambda) = lambda {
                c.name_closure(lambda, c.text(name));
            }
        }
    }

    fn invocation(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        match function.kind() {
            "identifier" => c.call(CallSpec {
                name: c.text(function).to_owned(),
                kind: CallKind::Function,
                receiver: Receiver::None,
                range: (function.start_byte(), function.end_byte()),
            }),
            "generic_name" => {
                if let Some(name) = function.named_child(0) {
                    c.call(CallSpec {
                        name: c.text(name).to_owned(),
                        kind: CallKind::Function,
                        receiver: Receiver::None,
                        range: (name.start_byte(), name.end_byte()),
                    });
                }
            }
            "member_access_expression" => {
                let Some(name) = function.child_by_field_name("name") else {
                    return dynamic_call(c, function);
                };
                let name = if name.kind() == "generic_name" {
                    name.named_child(0).unwrap_or(name)
                } else {
                    name
                };
                let receiver = match function.child_by_field_name("expression") {
                    Some(expression) => receiver_from(expression, c.src),
                    None => {
                        // `this` and `base` are anonymous keywords in this grammar.
                        let text = c.text(function);
                        if text.starts_with("base.") {
                            Receiver::Super
                        } else if text.starts_with("this.") {
                            Receiver::SelfValue
                        } else {
                            Receiver::Expr
                        }
                    }
                };
                c.call(CallSpec {
                    name: c.text(name).to_owned(),
                    kind: CallKind::Method,
                    receiver,
                    range: (name.start_byte(), name.end_byte()),
                });
            }
            _ => dynamic_call(c, function),
        }
    }

    fn creation(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        let Some(segments) = path_segments(ty, c.src) else {
            return dynamic_call(c, ty);
        };
        let Some((name, prefix)) = segments.split_last() else {
            return;
        };
        let last = match ty.kind() {
            "qualified_name" => ty.child_by_field_name("name").unwrap_or(ty),
            "generic_name" => ty.named_child(0).unwrap_or(ty),
            _ => ty,
        };
        c.call(CallSpec {
            name: name.clone(),
            kind: CallKind::Constructor,
            receiver: if prefix.is_empty() {
                Receiver::None
            } else {
                Receiver::Path(prefix.to_vec())
            },
            range: (last.start_byte(), last.end_byte()),
        });
    }
}

// ----- resolution -----

pub(crate) struct CSharpScheme;

fn namespace_files<'a>(
    ctx: &'a ResolutionContext<'_>,
    namespace: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    ctx.project
        .files()
        .filter(move |(_, file)| {
            file.language == LanguageId::CSharp
                && file.types.iter().any(|t| t.namespace == namespace)
        })
        .map(|(path, _)| path)
}

impl Scheme for CSharpScheme {
    fn language(&self) -> LanguageId {
        LanguageId::CSharp
    }

    fn bare_call_sees_enclosing_type(&self) -> bool {
        true
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        // `using Alias = Ns.Type;` names a type directly.
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Namespace && i.local == name)
        {
            let span = project_span(ctx.path, import.start, import.end);
            let (namespace, type_name) = import
                .module
                .rsplit_once('.')
                .unwrap_or(("", import.module.as_str()));
            for path in namespace_files(ctx, namespace) {
                let declares = ctx.project.file(path).is_some_and(|f| {
                    f.types
                        .iter()
                        .any(|t| t.name == type_name && t.namespace == namespace)
                });
                if declares {
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: type_name.to_owned(),
                        via_type: None,
                        namespace: None,
                        rule: "using-alias",
                        span: span.clone(),
                    });
                }
            }
        }
        // `using static Ns.Type;` exposes the type's static members by simple name.
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Static)
        {
            let span = project_span(ctx.path, import.start, import.end);
            let (namespace, type_name) = import
                .module
                .rsplit_once('.')
                .unwrap_or(("", import.module.as_str()));
            for path in namespace_files(ctx, namespace) {
                let declares = ctx.project.file(path).is_some_and(|f| {
                    f.types
                        .iter()
                        .any(|t| t.name == type_name && t.namespace == namespace)
                });
                if declares {
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: name.to_owned(),
                        via_type: Some(type_name.to_owned()),
                        namespace: None,
                        rule: "using-static",
                        span: span.clone(),
                    });
                }
            }
        }
        lookup
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let [alias] = path else { return lookup };
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Namespace && &i.local == alias)
        {
            let span = project_span(ctx.path, import.start, import.end);
            for file in namespace_files(ctx, &import.module) {
                lookup.hits.push(ModuleHit {
                    path: file.to_owned(),
                    namespace: Some(import.module.clone()),
                    rule: "using-alias",
                    span: span.clone(),
                });
            }
        }
        lookup
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut namespaces: Vec<(String, &'static str, Option<crate::model::SourceSpan>)> =
            Vec::new();
        let call_ns = ctx
            .file
            .package
            .clone()
            .or_else(|| ctx.file.types.first().map(|t| t.namespace.clone()))
            .unwrap_or_default();
        // The enclosing namespace chain is always visible.
        let mut current = call_ns.as_str();
        loop {
            namespaces.push((current.to_owned(), "same-namespace", None));
            match current.rsplit_once('.') {
                Some((parent, _)) => current = parent,
                None => break,
            }
        }
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
        {
            namespaces.push((
                import.module.clone(),
                "using-namespace",
                Some(project_span(ctx.path, import.start, import.end)),
            ));
        }
        let mut peers = Vec::new();
        for (namespace, rule, span) in namespaces {
            for path in namespace_files(ctx, &namespace) {
                if path != ctx.path {
                    peers.push(Peer {
                        path: path.to_owned(),
                        namespace: Some(namespace.clone()),
                        functions: false,
                        rule,
                        span: span.clone(),
                    });
                }
            }
        }
        peers
    }
}
