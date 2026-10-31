//! Go: package scope by directory, import paths matched against workspace directories, methods
//! through receivers, composite-literal typed locals and explicit type-conversion handling.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, project_span, BareTypeCall, ImportHit, ImportLookup, ModuleHit, ModuleLookup, Peer,
    ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Go;

impl Syntax for Go {
    fn language(&self) -> LanguageId {
        LanguageId::Go
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves package scope (directory plus package clause) and import paths are matched to workspace directories by path suffix (go.mod is not read)".into(),
            "supports functions, methods through named receivers, embedded-struct promotion, composite-literal typed locals, and `T(x)` conversions of workspace types (reported as non-calls)".into(),
            "interface method calls, function values, generics instantiation, cgo, and build-tag file selection stay unresolved".into(),
            "imports outside the workspace (standard library and modules) are reported as external targets".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "package_clause" => {
                let mut cursor = node.walk();
                let name = node.named_children(&mut cursor).next();
                if let Some(name) = name {
                    c.set_package(c.text(name).to_owned());
                }
                Action::Skip
            }
            "import_spec" => {
                self.import(node, c);
                Action::Skip
            }
            "type_spec" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Descend;
                };
                let ty = node.child_by_field_name("type");
                let kind = match ty.map(|t| t.kind()) {
                    Some("interface_type") => TypeKind::Interface,
                    _ => TypeKind::Struct,
                };
                let mut bases = Vec::new();
                if let Some(ty) = ty.filter(|t| t.kind() == "struct_type") {
                    embedded(ty, c.src, &mut bases);
                }
                Action::Type(TypeSpec {
                    name: c.text(name).to_owned(),
                    name_range: (name.start_byte(), name.end_byte()),
                    kind,
                    bases,
                })
            }
            "function_declaration" => {
                let name = node.child_by_field_name("name");
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: CallableKind::Function,
                    container: None,
                    namespace: None,
                    params: params(node.child_by_field_name("parameters"), c),
                })
            }
            "method_declaration" => {
                let name = node.child_by_field_name("name");
                let mut all = Vec::new();
                let mut container = None;
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    let mut cursor = receiver.walk();
                    for declaration in receiver.named_children(&mut cursor) {
                        let ty = declaration
                            .child_by_field_name("type")
                            .and_then(|t| simple_type(c.text(t)))
                            .map(|t| last_segment(&t).to_owned());
                        container = ty.clone();
                        if let Some(var) = declaration.child_by_field_name("name") {
                            all.push((c.text(var).to_owned(), ty));
                        }
                    }
                }
                all.extend(params(node.child_by_field_name("parameters"), c));
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: CallableKind::Method,
                    container,
                    namespace: None,
                    params: all,
                })
            }
            "func_literal" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: params(node.child_by_field_name("parameters"), c),
            }),
            "short_var_declaration" => {
                declare_pairs(
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                    None,
                    c,
                );
                Action::Descend
            }
            "var_spec" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| simple_type(c.text(t)));
                let mut cursor = node.walk();
                let names: Vec<_> = node.children_by_field_name("name", &mut cursor).collect();
                for name in names {
                    c.declare_typed(c.text(name), ty.clone(), false, false);
                }
                if let Some(values) = node.child_by_field_name("value") {
                    let mut cursor = node.walk();
                    let names: Vec<_> = node.children_by_field_name("name", &mut cursor).collect();
                    let mut inner = values.walk();
                    for (name, value) in names.iter().zip(values.named_children(&mut inner)) {
                        if value.kind() == "func_literal" {
                            c.name_closure(value, c.text(*name));
                        }
                    }
                }
                Action::Descend
            }
            "range_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut names = Vec::new();
                    binding_names(left, c.src, &mut names);
                    for name in names {
                        c.declare(&name, false);
                    }
                }
                Action::Descend
            }
            "call_expression" => {
                self.call(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn embedded(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for list in node.named_children(&mut cursor) {
        if list.kind() != "field_declaration_list" {
            continue;
        }
        let mut inner = list.walk();
        for field in list.named_children(&mut inner) {
            if field.kind() == "field_declaration" && field.child_by_field_name("name").is_none() {
                if let Some(ty) = field.child_by_field_name("type") {
                    if let Some(simple) = simple_type(&src[ty.byte_range()]) {
                        out.push(last_segment(&simple).to_owned());
                    }
                }
            }
        }
    }
}

fn params(list: Option<Node<'_>>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(list) = list else { return out };
    let mut cursor = list.walk();
    for declaration in list.named_children(&mut cursor) {
        let ty = declaration
            .child_by_field_name("type")
            .and_then(|t| simple_type(c.text(t)));
        let mut inner = declaration.walk();
        for name in declaration.children_by_field_name("name", &mut inner) {
            out.push((c.text(name).to_owned(), ty.clone()));
        }
    }
    out
}

fn declare_pairs(
    left: Option<Node<'_>>,
    right: Option<Node<'_>>,
    declared: Option<String>,
    c: &mut Collector<'_>,
) {
    let (Some(left), right) = (left, right) else {
        return;
    };
    let mut cursor = left.walk();
    let names: Vec<_> = left.named_children(&mut cursor).collect();
    let values: Vec<_> = right
        .map(|r| {
            let mut inner = r.walk();
            r.named_children(&mut inner).collect()
        })
        .unwrap_or_default();
    for (index, name) in names.iter().enumerate() {
        if name.kind() != "identifier" {
            continue;
        }
        let text = c.text(*name).to_owned();
        let value = values.get(index).copied();
        let literal = value.and_then(|value| literal_type(value, c.src));
        match literal {
            Some(ty) => c.declare_typed(&text, Some(ty), true, false),
            None => c.declare_typed(&text, declared.clone(), false, false),
        }
        if let Some(value) = value.filter(|v| v.kind() == "func_literal") {
            c.name_closure(value, &text);
        }
    }
}

/// Type of `T{...}` or `&T{...}`.
fn literal_type(node: Node<'_>, src: &str) -> Option<String> {
    let literal = match node.kind() {
        "composite_literal" => node,
        "unary_expression" => node
            .child_by_field_name("operand")
            .filter(|n| n.kind() == "composite_literal")?,
        _ => return None,
    };
    let ty = literal.child_by_field_name("type")?;
    simple_type(&src[ty.byte_range()])
}

impl Go {
    fn import(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(path) = node.child_by_field_name("path") else {
            return;
        };
        let module = unquote(c.text(path)).to_owned();
        let alias = node
            .child_by_field_name("name")
            .map(|n| c.text(n).to_owned());
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let (local, imported, kind) = match alias.as_deref() {
            Some("_") => return,
            Some(".") => ("*".to_owned(), "*".to_owned(), ImportKind::Wildcard),
            Some(alias) => (alias.to_owned(), "*".to_owned(), ImportKind::Namespace),
            // The bound name is the imported package's own name, resolved later.
            None => (
                module.rsplit('/').next().unwrap_or(&module).to_owned(),
                String::new(),
                ImportKind::Namespace,
            ),
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

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
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
            "selector_expression" => {
                let (Some(operand), Some(field)) = (
                    function.child_by_field_name("operand"),
                    function.child_by_field_name("field"),
                ) else {
                    return dynamic_call(c, function);
                };
                c.call(CallSpec {
                    name: c.text(field).to_owned(),
                    kind: CallKind::Method,
                    receiver: receiver_from(operand, c.src),
                    range: (field.start_byte(), field.end_byte()),
                });
            }
            _ => dynamic_call(c, function),
        }
    }
}

// ----- resolution -----

pub(crate) struct GoScheme;

/// Go files of the directory whose import path ends with `import_path`, with their package.
fn package_dir<'a>(
    ctx: &'a ResolutionContext<'_>,
    import_path: &str,
) -> Option<(String, Vec<&'a str>, String)> {
    let mut best: Option<String> = None;
    for (path, file) in ctx.project.files() {
        if file.language != LanguageId::Go {
            continue;
        }
        let dir = dirname(path);
        if dir.is_empty() {
            continue;
        }
        let matches = import_path == dir || import_path.ends_with(&format!("/{dir}"));
        if matches
            && best
                .as_ref()
                .is_none_or(|current| dir.len() > current.len())
        {
            best = Some(dir.to_owned());
        }
    }
    let dir = best?;
    let files: Vec<&str> = ctx
        .project
        .files()
        .filter(|(path, file)| {
            file.language == LanguageId::Go
                && dirname(path) == dir
                && file
                    .package
                    .as_deref()
                    .is_some_and(|p| !p.ends_with("_test"))
        })
        .map(|(path, _)| path)
        .collect();
    let package = files
        .first()
        .and_then(|path| ctx.project.file(path))
        .and_then(|file| file.package.clone())?;
    Some((dir, files, package))
}

impl Scheme for GoScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Go
    }

    fn bare_type_call(&self) -> BareTypeCall {
        BareTypeCall::Conversion
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let dir = dirname(ctx.path);
        ctx.project
            .files()
            .filter(|(path, file)| {
                *path != ctx.path
                    && file.language == LanguageId::Go
                    && dirname(path) == dir
                    && file.package == ctx.file.package
            })
            .map(|(path, _)| Peer {
                path: path.to_owned(),
                namespace: ctx.file.package.clone(),
                functions: true,
                rule: "same-package",
                span: None,
            })
            .collect()
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let [name] = path else { return lookup };
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Namespace)
        {
            let span = project_span(ctx.path, import.start, import.end);
            match package_dir(ctx, &import.module) {
                Some((_, files, package)) => {
                    let bound = if import.imported == "*" {
                        import.local.clone()
                    } else {
                        package.clone()
                    };
                    if &bound == name {
                        for file in files {
                            lookup.hits.push(ModuleHit {
                                path: file.to_owned(),
                                namespace: Some(package.clone()),
                                rule: "package-import",
                                span: span.clone(),
                            });
                        }
                    }
                }
                None => {
                    let bound = if import.imported == "*" {
                        import.local.clone()
                    } else {
                        import
                            .module
                            .rsplit('/')
                            .next()
                            .unwrap_or(&import.module)
                            .to_owned()
                    };
                    if &bound == name {
                        lookup.external = Some((import.module.clone(), span));
                    }
                }
            }
        }
        lookup
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        // Dot imports make every exported name of the package visible unqualified.
        let mut lookup = ImportLookup::default();
        for import in ctx
            .file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
        {
            let span = project_span(ctx.path, import.start, import.end);
            if let Some((_, files, package)) = package_dir(ctx, &import.module) {
                for file in files {
                    lookup.hits.push(ImportHit {
                        path: file.to_owned(),
                        member: name.to_owned(),
                        via_type: None,
                        namespace: Some(package.clone()),
                        rule: "dot-import",
                        span: span.clone(),
                    });
                }
            }
        }
        lookup
    }
}
