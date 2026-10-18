//! Kotlin: packages, named/aliased/wildcard imports, top-level functions, classes, objects and
//! companion objects, primary and secondary constructors, and receivers typed by declarations or
//! by a construction. Kotlin and Java files of a package see each other, as on the JVM.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    project_span, BareTypeCall, ImportHit, ImportLookup, Peer, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Kotlin;

impl Syntax for Kotlin {
    fn language(&self) -> LanguageId {
        LanguageId::Kotlin
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves package-scoped and imported types, functions and objects (Kotlin or Java), aliased and wildcard imports, `Type(...)` constructions, companion-object calls, `this`/`super` calls, and receivers typed by declarations or constructions".into(),
            "a call `Name(...)` whose name is a visible type is a construction; classes without a primary or secondary constructor stay unresolved".into(),
            "extension functions, infix and operator calls, scope functions (`let`, `apply`, ...), smart casts, delegated properties, receivers of property type, and `this(...)`/`super(...)` delegation calls are not resolved".into(),
            "one-line class bodies such as `class A { fun b() }` are a parse error in the grammar in use; the calls inside them are withheld".into(),
            "imports of packages outside the workspace are reported as external targets".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "class_declaration" | "object_declaration" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Descend;
                };
                Action::Type(TypeSpec {
                    name: c.text(name).to_owned(),
                    name_range: (name.start_byte(), name.end_byte()),
                    kind: type_kind(node, c.src),
                    bases: bases(node, c.src),
                })
            }
            "package_header" => {
                if let Some(name) =
                    child_of(node, "qualified_identifier").or_else(|| child_of(node, "identifier"))
                {
                    c.set_package(c.text(name).to_owned());
                }
                Action::Skip
            }
            "import" => {
                import(node, c);
                Action::Skip
            }
            "function_declaration" => {
                let name = node.child_by_field_name("name");
                let in_type = c.current_type().is_some() && !c.in_callable();
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: if in_type {
                        CallableKind::Method
                    } else {
                        CallableKind::Function
                    },
                    container: None,
                    namespace: None,
                    params: params(child_of(node, "function_value_parameters"), c),
                })
            }
            "primary_constructor" | "secondary_constructor" => {
                let keyword = if node.kind() == "secondary_constructor" {
                    first_token(node, "constructor")
                } else {
                    None
                };
                let list = child_of(node, "class_parameters")
                    .or_else(|| child_of(node, "function_value_parameters"));
                Action::Callable(CallableSpec {
                    name: c.current_type().map(str::to_owned),
                    name_range: Some(keyword.map_or((node.start_byte(), node.end_byte()), |k| {
                        (k.start_byte(), k.end_byte())
                    })),
                    kind: CallableKind::Constructor,
                    container: None,
                    namespace: None,
                    params: params(list, c),
                })
            }
            "lambda_literal" | "anonymous_function" => {
                let list = child_of(node, "lambda_parameters")
                    .or_else(|| child_of(node, "function_value_parameters"));
                let mut params = params(list, c);
                if params.is_empty() && node.kind() == "lambda_literal" {
                    params.push(("it".into(), None));
                }
                Action::Callable(CallableSpec {
                    name: None,
                    name_range: None,
                    kind: CallableKind::Closure,
                    container: None,
                    namespace: None,
                    params,
                })
            }
            "property_declaration" => {
                locals(node, c);
                Action::Descend
            }
            "for_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if matches!(
                        child.kind(),
                        "variable_declaration" | "multi_variable_declaration"
                    ) {
                        declare_variables(child, c);
                    }
                }
                Action::Descend
            }
            "catch_block" => {
                if let Some(name) = child_of(node, "identifier") {
                    let ty = child_of(node, "user_type").and_then(|t| simple_type(c.text(t)));
                    c.declare_typed(c.text(name), ty, false, false);
                }
                Action::Descend
            }
            "call_expression" => {
                call(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn child_of<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|n| n.kind() == kind);
    found
}

fn first_token<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|n| n.kind() == kind);
    found
}

fn type_kind(node: Node<'_>, src: &str) -> TypeKind {
    if first_token(node, "interface").is_some() {
        return TypeKind::Interface;
    }
    let is_enum = child_of(node, "modifiers")
        .is_some_and(|m| src[m.byte_range()].split_whitespace().any(|w| w == "enum"));
    if is_enum {
        TypeKind::Enum
    } else {
        TypeKind::Class
    }
}

/// The last name of each supertype in `: Base(), Interface`.
fn bases(node: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(specifiers) = child_of(node, "delegation_specifiers") else {
        return out;
    };
    let mut cursor = specifiers.walk();
    for specifier in specifiers.named_children(&mut cursor) {
        let ty = child_of(specifier, "user_type").or_else(|| {
            child_of(specifier, "constructor_invocation").and_then(|i| child_of(i, "user_type"))
        });
        if let Some(name) = ty
            .and_then(|t| segments(t, src))
            .and_then(|s| s.last().cloned())
        {
            out.push(name);
        }
    }
    out
}

/// Segments of a name expression (`a.b.C`, `this.x`, a user type), or `None` for anything else.
fn segments(node: Node<'_>, src: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![src[node.byte_range()].to_owned()]),
        "this_expression" => Some(vec!["this".into()]),
        "super_expression" => Some(vec!["super".into()]),
        "parenthesized_expression" => node.named_child(0).and_then(|n| segments(n, src)),
        "navigation_expression" => {
            let mut parts = segments(node.named_child(0)?, src)?;
            let last = node.named_child(node.named_child_count().checked_sub(1)?)?;
            if last.kind() != "identifier" || node.named_child_count() != 2 {
                return None;
            }
            parts.push(src[last.byte_range()].to_owned());
            Some(parts)
        }
        "qualified_identifier" | "user_type" => {
            let mut cursor = node.walk();
            let parts: Vec<String> = node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "identifier")
                .map(|n| src[n.byte_range()].to_owned())
                .collect();
            (!parts.is_empty()).then_some(parts)
        }
        _ => None,
    }
}

fn receiver(node: Node<'_>, src: &str) -> Receiver {
    match segments(node, src) {
        Some(parts) if parts.len() == 1 && parts[0] == "this" => Receiver::SelfValue,
        Some(parts) if parts.len() == 1 && parts[0] == "super" => Receiver::Super,
        Some(parts) if parts[0] == "this" || parts[0] == "super" => Receiver::Expr,
        Some(parts) => Receiver::Path(parts),
        None => Receiver::Expr,
    }
}

/// Parameter names with their declared simple types.
fn params(list: Option<Node<'_>>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(list) = list else {
        return out;
    };
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        if !matches!(
            param.kind(),
            "parameter" | "class_parameter" | "variable_declaration"
        ) {
            continue;
        }
        let Some(name) = child_of(param, "identifier") else {
            continue;
        };
        out.push((c.text(name).to_owned(), declared_type(param, c.src)));
    }
    out
}

fn declared_type(node: Node<'_>, src: &str) -> Option<String> {
    let ty = child_of(node, "user_type")
        .or_else(|| child_of(node, "nullable_type").and_then(|n| child_of(n, "user_type")))?;
    simple_type(&src[ty.byte_range()])
}

fn declare_variables(node: Node<'_>, c: &mut Collector<'_>) {
    if node.kind() == "multi_variable_declaration" {
        let mut cursor = node.walk();
        let parts: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for part in parts {
            declare_variables(part, c);
        }
        return;
    }
    if let Some(name) = child_of(node, "identifier") {
        let ty = declared_type(node, c.src);
        c.declare_typed(c.text(name), ty, false, false);
    }
}

/// `val x = Type(...)`, `val x: Type = ...` and destructuring declarations.
fn locals(node: Node<'_>, c: &mut Collector<'_>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let Some(declaration) = children.iter().find(|n| {
        matches!(
            n.kind(),
            "variable_declaration" | "multi_variable_declaration"
        )
    }) else {
        return;
    };
    if declaration.kind() == "multi_variable_declaration" {
        return declare_variables(*declaration, c);
    }
    let Some(name) = child_of(*declaration, "identifier") else {
        return;
    };
    let name = c.text(name);
    let value = children
        .iter()
        .rev()
        .find(|n| n.id() != declaration.id() && n.kind() != "modifiers");
    if let Some(value) = value.filter(|v| v.kind() == "lambda_literal") {
        c.name_closure(*value, name);
    }
    // `Type(...)` constructs a `Type` when the name starts with a capital, as types do.
    let created = value
        .filter(|v| v.kind() == "call_expression")
        .and_then(|v| v.named_child(0))
        .filter(|callee| matches!(callee.kind(), "identifier" | "navigation_expression"))
        .and_then(|callee| segments(callee, c.src))
        .and_then(|parts| parts.last().cloned())
        .filter(|last| last.starts_with(|ch: char| ch.is_ascii_uppercase()));
    match (declared_type(*declaration, c.src), created) {
        (Some(ty), _) => c.declare_typed(name, Some(ty), false, false),
        (None, Some(ty)) => c.declare_typed(name, Some(ty), true, false),
        (None, None) => c.declare_typed(name, None, false, false),
    }
}

fn call(node: Node<'_>, c: &mut Collector<'_>) {
    let Some(callee) = node.named_child(0) else {
        return;
    };
    match callee.kind() {
        "identifier" => c.call(CallSpec {
            name: c.text(callee).to_owned(),
            kind: CallKind::Function,
            receiver: Receiver::None,
            range: (callee.start_byte(), callee.end_byte()),
        }),
        "navigation_expression" => {
            let count = callee.named_child_count();
            let (Some(object), Some(name)) = (
                callee.named_child(0),
                count.checked_sub(1).and_then(|i| callee.named_child(i)),
            ) else {
                return dynamic_call(c, callee);
            };
            if name.kind() != "identifier" || count != 2 {
                return dynamic_call(c, callee);
            }
            c.call(CallSpec {
                name: c.text(name).to_owned(),
                kind: CallKind::Method,
                receiver: receiver(object, c.src),
                range: (name.start_byte(), name.end_byte()),
            });
        }
        _ => dynamic_call(c, callee),
    }
}

fn import(node: Node<'_>, c: &mut Collector<'_>) {
    let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
    let Some(path) =
        child_of(node, "qualified_identifier").or_else(|| child_of(node, "identifier"))
    else {
        return;
    };
    let Some(parts) = segments(path, c.src) else {
        return;
    };
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    if children.iter().any(|n| n.kind() == "*") {
        c.import(ImportFact {
            local: "*".into(),
            imported: "*".into(),
            module: parts.join("."),
            kind: ImportKind::Wildcard,
            type_only: false,
            start,
            end,
        });
        return;
    }
    let Some((last, rest)) = parts.split_last() else {
        return;
    };
    let alias = children
        .iter()
        .skip_while(|n| n.kind() != "as")
        .find(|n| n.kind() == "identifier")
        .map(|n| c.text(*n).to_owned());
    c.import(ImportFact {
        local: alias.unwrap_or_else(|| last.clone()),
        imported: last.clone(),
        module: rest.join("."),
        kind: ImportKind::Named,
        type_only: false,
        start,
        end,
    });
}

// ----- resolution -----

pub(crate) struct KotlinScheme;

/// Java and Kotlin files declaring `package`: the two languages share packages on the JVM.
pub(crate) fn jvm_package_files<'a>(
    ctx: &'a ResolutionContext<'_>,
    package: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    ctx.project
        .files()
        .filter(move |(_, file)| {
            matches!(file.language, LanguageId::Java | LanguageId::Kotlin)
                && file.package.as_deref() == Some(package)
        })
        .map(|(path, _)| path)
}

impl KotlinScheme {
    fn declares_type(ctx: &ResolutionContext<'_>, path: &str, name: &str) -> bool {
        ctx.project
            .file(path)
            .is_some_and(|f| f.types.iter().any(|t| t.name == name))
    }

    fn declares_function(ctx: &ResolutionContext<'_>, path: &str, name: &str) -> bool {
        ctx.project.file(path).is_some_and(|f| {
            f.callables.iter().any(|item| {
                item.parent.is_none()
                    && item.container.is_none()
                    && item.kind == CallableKind::Function
                    && item.name == name
            })
        })
    }
}

impl Scheme for KotlinScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Kotlin
    }

    fn bare_call_sees_enclosing_type(&self) -> bool {
        true
    }

    fn bare_type_call(&self) -> BareTypeCall {
        BareTypeCall::Construct
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.local == name && i.kind == ImportKind::Named)
        {
            let span = project_span(ctx.path, import.start, import.end);
            let mut found = false;
            for path in jvm_package_files(ctx, &import.module) {
                if Self::declares_type(ctx, path, &import.imported)
                    || Self::declares_function(ctx, path, &import.imported)
                {
                    found = true;
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: import.imported.clone(),
                        via_type: None,
                        namespace: None,
                        rule: "named-import",
                        span: span.clone(),
                    });
                }
            }
            if found {
                continue;
            }
            // `import pkg.Object.member`: a member of an object or companion, like a static import.
            let (package, owner) = import
                .module
                .rsplit_once('.')
                .unwrap_or(("", import.module.as_str()));
            for path in jvm_package_files(ctx, package) {
                if Self::declares_type(ctx, path, owner) {
                    found = true;
                    lookup.hits.push(ImportHit {
                        path: path.to_owned(),
                        member: import.imported.clone(),
                        via_type: Some(owner.to_owned()),
                        namespace: None,
                        rule: "member-import",
                        span: span.clone(),
                    });
                }
            }
            if !found
                && jvm_package_files(ctx, &import.module).next().is_none()
                && jvm_package_files(ctx, package).next().is_none()
            {
                lookup.external = Some((format!("{}.{}", import.module, import.imported), span));
            }
        }
        lookup
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut peers = Vec::new();
        match ctx.file.package.as_deref() {
            Some(package) => {
                for path in jvm_package_files(ctx, package) {
                    if path != ctx.path {
                        peers.push(Peer {
                            path: path.to_owned(),
                            namespace: Some(package.to_owned()),
                            functions: true,
                            rule: "same-package",
                            span: None,
                        });
                    }
                }
            }
            None => {
                let directory = ctx.path.rsplit_once('/').map(|x| x.0);
                for (path, file) in ctx.project.files() {
                    if path != ctx.path
                        && matches!(file.language, LanguageId::Java | LanguageId::Kotlin)
                        && file.package.is_none()
                        && path.rsplit_once('/').map(|x| x.0) == directory
                    {
                        peers.push(Peer {
                            path: path.to_owned(),
                            namespace: None,
                            functions: true,
                            rule: "same-package",
                            span: None,
                        });
                    }
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
            for path in jvm_package_files(ctx, &import.module) {
                peers.push(Peer {
                    path: path.to_owned(),
                    namespace: Some(import.module.clone()),
                    functions: true,
                    rule: "wildcard-import",
                    span: Some(span.clone()),
                });
            }
        }
        peers
    }
}
