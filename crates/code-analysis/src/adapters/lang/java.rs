//! Java: packages, single-type/wildcard/static imports, constructors, overloads and concrete
//! receivers. Virtual dispatch keeps ambiguity instead of choosing a target.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    project_span, ImportHit, ImportLookup, Peer, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Java;

const TYPE_KINDS: [(&str, TypeKind); 5] = [
    ("class_declaration", TypeKind::Class),
    ("interface_declaration", TypeKind::Interface),
    ("enum_declaration", TypeKind::Enum),
    ("record_declaration", TypeKind::Class),
    ("annotation_type_declaration", TypeKind::Interface),
];

impl Syntax for Java {
    fn language(&self) -> LanguageId {
        LanguageId::Java
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves package-scoped types, single-type/wildcard/static imports, constructors, static calls, `this`/`super` calls, and receivers typed by `new` or declarations".into(),
            "overloads and overriding subtypes are reported as possible targets, never as one exact target".into(),
            "reflection, method references, generics-based dispatch, anonymous-class members, and explicit `this(...)`/`super(...)` constructor calls are not resolved".into(),
            "imports of packages outside the workspace are reported as external targets".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        let kind = node.kind();
        if let Some((_, type_kind)) = TYPE_KINDS.iter().find(|(name, _)| *name == kind) {
            let Some(name) = node.child_by_field_name("name") else {
                return Action::Descend;
            };
            let mut bases = Vec::new();
            for field in ["superclass", "interfaces"] {
                if let Some(clause) = node.child_by_field_name(field) {
                    collect_types(clause, c.src, &mut bases);
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "extends_interfaces" {
                    collect_types(child, c.src, &mut bases);
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
            "package_declaration" => {
                let mut cursor = node.walk();
                let name = node
                    .named_children(&mut cursor)
                    .find(|n| matches!(n.kind(), "scoped_identifier" | "identifier"));
                if let Some(name) = name {
                    c.set_package(c.text(name).to_owned());
                }
                Action::Skip
            }
            "import_declaration" => {
                self.import(node, c);
                Action::Skip
            }
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => {
                let name = node.child_by_field_name("name");
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: if kind == "method_declaration" {
                        CallableKind::Method
                    } else {
                        CallableKind::Constructor
                    },
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            "lambda_expression" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: lambda_params(node, c),
            }),
            "local_variable_declaration" => {
                self.locals(node, c);
                Action::Descend
            }
            "enhanced_for_statement" | "catch_formal_parameter" | "resource" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| simple_type(c.text(t)));
                    c.declare_typed(c.text(name), ty, false, false);
                }
                Action::Descend
            }
            "method_invocation" => {
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

fn collect_types(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "type_identifier" => out.push(src[node.byte_range()].to_owned()),
        "scoped_type_identifier" => {
            if let Some(last) = node.named_child(node.named_child_count().saturating_sub(1)) {
                out.push(src[last.byte_range()].to_owned());
            }
        }
        "type_arguments" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_types(child, src, out);
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
        match child.kind() {
            "formal_parameter" | "spread_parameter" => {
                let mut name = child.child_by_field_name("name");
                if name.is_none() {
                    let mut inner = child.walk();
                    let declarator = child
                        .named_children(&mut inner)
                        .find(|n| n.kind() == "variable_declarator");
                    name = declarator.and_then(|d| d.child_by_field_name("name"));
                }
                let ty = child
                    .child_by_field_name("type")
                    .and_then(|t| simple_type(c.text(t)));
                if let Some(name) = name {
                    out.push((c.text(name).to_owned(), ty));
                }
            }
            "identifier" => out.push((c.text(child).to_owned(), None)),
            _ => {}
        }
    }
    out
}

fn lambda_params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    if parameters.kind() == "identifier" {
        return vec![(c.text(parameters).to_owned(), None)];
    }
    let mut names = Vec::new();
    binding_names(parameters, c.src, &mut names);
    names.into_iter().map(|n| (n, None)).collect()
}

impl Java {
    fn import(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let text = c.text(node);
        let is_static = text.trim_start().starts_with("import static");
        let wildcard = text
            .trim_end()
            .trim_end_matches(';')
            .trim_end()
            .ends_with('*');
        let mut cursor = node.walk();
        let Some(path) = node
            .named_children(&mut cursor)
            .find(|n| matches!(n.kind(), "scoped_identifier" | "identifier"))
        else {
            return;
        };
        let segments = path_segments(path, c.src).unwrap_or_default();
        let (module, member) = match (wildcard, segments.split_last()) {
            (true, _) => (segments.join("."), "*".to_owned()),
            (false, Some((last, rest))) => (rest.join("."), last.clone()),
            (false, None) => return,
        };
        let kind = match (is_static, wildcard) {
            (true, _) => ImportKind::Static,
            (false, true) => ImportKind::Wildcard,
            (false, false) => ImportKind::Named,
        };
        c.import(ImportFact {
            local: member.clone(),
            imported: member,
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
        for declarator in node.children_by_field_name("declarator", &mut cursor) {
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            let created = declarator
                .child_by_field_name("value")
                .filter(|v| v.kind() == "object_creation_expression")
                .and_then(|v| v.child_by_field_name("type"))
                .and_then(|t| simple_type(c.text(t)));
            match created {
                Some(ty) => c.declare_typed(c.text(name), Some(ty), true, false),
                None => c.declare_typed(c.text(name), declared.clone(), false, false),
            }
        }
    }

    fn invocation(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let (kind, receiver) = match node.child_by_field_name("object") {
            None => (CallKind::Function, Receiver::None),
            Some(object) => (CallKind::Method, receiver_from(object, c.src)),
        };
        c.call(CallSpec {
            name: c.text(name).to_owned(),
            kind,
            receiver,
            range: (name.start_byte(), name.end_byte()),
        });
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
        let last = ty_last_node(ty);
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

fn ty_last_node(node: Node<'_>) -> Node<'_> {
    match node.kind() {
        "generic_type" => node.named_child(0).map(ty_last_node).unwrap_or(node),
        "scoped_type_identifier" => node
            .named_child(node.named_child_count().saturating_sub(1))
            .map(ty_last_node)
            .unwrap_or(node),
        _ => node,
    }
}

// ----- resolution -----

pub(crate) struct JavaScheme;

/// Kotlin files of the package are visible too: both compile to the same JVM packages.
fn package_files<'a>(
    ctx: &'a ResolutionContext<'_>,
    package: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    super::kotlin::jvm_package_files(ctx, package)
}

impl Scheme for JavaScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Java
    }

    fn bare_call_sees_enclosing_type(&self) -> bool {
        true
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        for import in
            ctx.file.imports.iter().filter(|i| {
                i.local == name && matches!(i.kind, ImportKind::Named | ImportKind::Static)
            })
        {
            let span = project_span(ctx.path, import.start, import.end);
            if import.kind == ImportKind::Static {
                // `import static pkg.Type.member;` — module is `pkg.Type`.
                let (package, type_name) = import
                    .module
                    .rsplit_once('.')
                    .unwrap_or(("", import.module.as_str()));
                for path in package_files(ctx, package) {
                    let declares = ctx
                        .project
                        .file(path)
                        .is_some_and(|f| f.types.iter().any(|t| t.name == type_name));
                    if declares {
                        lookup.hits.push(ImportHit {
                            path: path.to_owned(),
                            member: import.imported.clone(),
                            via_type: Some(type_name.to_owned()),
                            namespace: None,
                            rule: "static-import",
                            span: span.clone(),
                        });
                    }
                }
                continue;
            }
            let mut found = false;
            for path in package_files(ctx, &import.module) {
                let declares = ctx
                    .project
                    .file(path)
                    .is_some_and(|f| f.types.iter().any(|t| t.name == import.imported));
                if declares {
                    found = true;
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: import.imported.clone(),
                        via_type: None,
                        namespace: None,
                        rule: "static-import",
                        span: span.clone(),
                    });
                }
            }
            if !found && package_files(ctx, &import.module).next().is_none() {
                lookup.external = Some((format!("{}.{}", import.module, import.imported), span));
            }
        }
        // `import static pkg.Type.*;` makes every static member of the type visible by name.
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Static && i.imported == "*")
        {
            let span = project_span(ctx.path, import.start, import.end);
            let (package, type_name) = import
                .module
                .rsplit_once('.')
                .unwrap_or(("", import.module.as_str()));
            for path in package_files(ctx, package) {
                let declares = ctx
                    .project
                    .file(path)
                    .is_some_and(|f| f.types.iter().any(|t| t.name == type_name));
                if declares {
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: name.to_owned(),
                        via_type: Some(type_name.to_owned()),
                        namespace: None,
                        rule: "static-wildcard-import",
                        span: span.clone(),
                    });
                }
            }
        }
        lookup
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut peers = Vec::new();
        if let Some(package) = ctx.file.package.as_deref() {
            for path in package_files(ctx, package) {
                if path != ctx.path {
                    peers.push(Peer {
                        path: path.to_owned(),
                        namespace: Some(package.to_owned()),
                        functions: false,
                        rule: "same-package",
                        span: None,
                    });
                }
            }
        } else {
            for (path, file) in ctx.project.files() {
                if path != ctx.path
                    && file.language == LanguageId::Java
                    && file.package.is_none()
                    && path.rsplit_once('/').map(|x| x.0) == ctx.path.rsplit_once('/').map(|x| x.0)
                {
                    peers.push(Peer {
                        path: path.to_owned(),
                        namespace: None,
                        functions: false,
                        rule: "same-package",
                        span: None,
                    });
                }
            }
        }
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
        {
            let span = project_span(ctx.path, import.start, import.end);
            for path in package_files(ctx, &import.module) {
                peers.push(Peer {
                    path: path.to_owned(),
                    namespace: Some(import.module.clone()),
                    functions: false,
                    rule: "wildcard-import",
                    span: Some(span.clone()),
                });
            }
        }
        peers
    }
}
