//! Python: `import`/`from` modules (including relative imports and package `__init__` re-exports),
//! classes with `__init__`, `self` calls, and typed locals from constructor calls.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, BareTypeCall, ImportHit, ImportLookup, ModuleHit,
    ModuleLookup, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Python;

impl Syntax for Python {
    fn language(&self) -> LanguageId {
        LanguageId::Python
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves `import`/`from ... import` of workspace modules and packages (absolute, relative, aliased, `__init__` re-exports, wildcard imports)".into(),
            "supports functions, classes, `__init__` construction, `self`/`cls` calls, and receivers typed by constructor calls or annotations".into(),
            "decorators, monkey patching, `getattr`, dynamic attributes, metaclasses, and `__all__` filtering are not modelled and stay unresolved".into(),
            "assignments shadow definitions function-wide (conditional definitions are not distinguished)".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "import_statement" => {
                self.import(node, c);
                Action::Skip
            }
            "import_from_statement" => {
                self.import_from(node, c);
                Action::Skip
            }
            "class_definition" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Descend;
                };
                let mut bases = Vec::new();
                if let Some(superclasses) = node.child_by_field_name("superclasses") {
                    let mut cursor = superclasses.walk();
                    for base in superclasses.named_children(&mut cursor) {
                        if let Some(path) = path_segments(base, c.src) {
                            bases.extend(path.last().cloned());
                        }
                    }
                }
                Action::Type(TypeSpec {
                    name: c.text(name).to_owned(),
                    name_range: (name.start_byte(), name.end_byte()),
                    kind: TypeKind::Class,
                    bases,
                })
            }
            "function_definition" => {
                let name = node.child_by_field_name("name");
                let text = name.map(|n| c.text(n).to_owned());
                let in_class = c.current_type().is_some() && !c.in_callable();
                let kind = match (in_class, text.as_deref()) {
                    (true, Some("__init__")) => CallableKind::Constructor,
                    (true, _) => CallableKind::Method,
                    (false, _) => CallableKind::Function,
                };
                Action::Callable(CallableSpec {
                    name: text,
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind,
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            "lambda" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: params(node, c),
            }),
            "assignment" => {
                self.assignment(node, c);
                Action::Descend
            }
            "for_statement" | "for_in_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    declare_all(left, c);
                }
                Action::Descend
            }
            "as_pattern" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    declare_all(alias, c);
                }
                Action::Descend
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    declare_all(name, c);
                }
                Action::Descend
            }
            "call" => {
                self.call(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn declare_all(node: Node<'_>, c: &mut Collector<'_>) {
    let mut names = Vec::new();
    binding_names(node, c.src, &mut names);
    for name in names {
        c.declare(&name, false);
    }
}

fn annotation(node: Option<Node<'_>>, src: &str) -> Option<String> {
    let text = &src[node?.byte_range()];
    simple_type(text.trim_start_matches(':').trim())
}

fn params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = parameters.walk();
    for child in parameters.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => out.push((c.text(child).to_owned(), None)),
            "typed_parameter" => {
                let mut inner = child.walk();
                let name = child.named_children(&mut inner).find(|n| {
                    matches!(
                        n.kind(),
                        "identifier" | "list_splat_pattern" | "dictionary_splat_pattern"
                    )
                });
                if let Some(name) = name {
                    let mut names = Vec::new();
                    binding_names(name, c.src, &mut names);
                    let ty = annotation(child.child_by_field_name("type"), c.src);
                    out.extend(names.into_iter().map(|n| (n, ty.clone())));
                }
            }
            "default_parameter" | "typed_default_parameter" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let ty = annotation(child.child_by_field_name("type"), c.src);
                    out.push((c.text(name).to_owned(), ty));
                }
            }
            _ => {
                let mut names = Vec::new();
                binding_names(child, c.src, &mut names);
                out.extend(names.into_iter().map(|n| (n, None)));
            }
        }
    }
    out
}

impl Python {
    fn assignment(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let right = node.child_by_field_name("right");
        if left.kind() != "identifier" {
            declare_all(left, c);
            return;
        }
        let name = c.text(left).to_owned();
        let annotated = annotation(node.child_by_field_name("type"), c.src);
        let constructed = right
            .filter(|value| value.kind() == "call")
            .and_then(|call| call.child_by_field_name("function"))
            .and_then(|function| path_segments(function, c.src))
            .and_then(|segments| segments.last().cloned())
            .filter(|segment| segment.chars().next().is_some_and(char::is_uppercase));
        match (constructed, annotated) {
            (Some(ty), _) => c.declare_typed(&name, Some(ty), true, false),
            (None, ty) => c.declare_typed(&name, ty, false, false),
        }
        if let Some(value) = right {
            if value.kind() == "lambda" {
                c.name_closure(value, &name);
            }
        }
    }

    fn import(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let mut cursor = node.walk();
        for name in node.children_by_field_name("name", &mut cursor) {
            match name.kind() {
                "aliased_import" => {
                    let (Some(module), Some(alias)) = (
                        name.child_by_field_name("name"),
                        name.child_by_field_name("alias"),
                    ) else {
                        continue;
                    };
                    c.import(ImportFact {
                        local: c.text(alias).to_owned(),
                        imported: "*".into(),
                        module: c.text(module).to_owned(),
                        kind: ImportKind::Namespace,
                        type_only: false,
                        start,
                        end,
                    });
                }
                "dotted_name" => {
                    let module = c.text(name).to_owned();
                    let local = module.split('.').next().unwrap_or(&module).to_owned();
                    c.import(ImportFact {
                        local,
                        imported: module.clone(),
                        module,
                        kind: ImportKind::Namespace,
                        type_only: false,
                        start,
                        end,
                    });
                }
                _ => {}
            }
        }
    }

    fn import_from(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(module) = node.child_by_field_name("module_name") else {
            return;
        };
        let module = c.text(module).to_owned();
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let type_only = false;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "wildcard_import" => c.import(ImportFact {
                    local: "*".into(),
                    imported: "*".into(),
                    module: module.clone(),
                    kind: ImportKind::Wildcard,
                    type_only,
                    start,
                    end,
                }),
                "aliased_import" => {
                    let (Some(name), Some(alias)) = (
                        child.child_by_field_name("name"),
                        child.child_by_field_name("alias"),
                    ) else {
                        continue;
                    };
                    c.import(ImportFact {
                        local: c.text(alias).to_owned(),
                        imported: c.text(name).to_owned(),
                        module: module.clone(),
                        kind: ImportKind::Named,
                        type_only,
                        start,
                        end,
                    });
                }
                "dotted_name"
                    if Some(child.id())
                        != node.child_by_field_name("module_name").map(|n| n.id()) =>
                {
                    let name = c.text(child).to_owned();
                    c.import(ImportFact {
                        local: name.clone(),
                        imported: name,
                        module: module.clone(),
                        kind: ImportKind::Named,
                        type_only,
                        start,
                        end,
                    });
                }
                _ => {}
            }
        }
    }

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        match function.kind() {
            "identifier" => {
                if c.text(function) == "super" {
                    return;
                }
                c.call(CallSpec {
                    name: c.text(function).to_owned(),
                    kind: CallKind::Function,
                    receiver: Receiver::None,
                    range: (function.start_byte(), function.end_byte()),
                });
            }
            "attribute" => {
                let (Some(object), Some(attribute)) = (
                    function.child_by_field_name("object"),
                    function.child_by_field_name("attribute"),
                ) else {
                    return dynamic_call(c, function);
                };
                let mut receiver = receiver_from(object, c.src);
                if object.kind() == "call"
                    && object
                        .child_by_field_name("function")
                        .is_some_and(|f| c.text(f) == "super")
                {
                    receiver = Receiver::Super;
                } else if c.text(object) == "cls" && c.current_type().is_some() {
                    receiver = Receiver::SelfValue;
                }
                c.call(CallSpec {
                    name: c.text(attribute).to_owned(),
                    kind: CallKind::Method,
                    receiver,
                    range: (attribute.start_byte(), attribute.end_byte()),
                });
            }
            _ => dynamic_call(c, function),
        }
    }
}

// ----- resolution -----

pub(crate) struct PythonScheme;

enum ModuleRef {
    File(String),
    External,
    Unknown,
}

impl PythonScheme {
    fn module_ref(&self, ctx: &ResolutionContext<'_>, module: &str) -> ModuleRef {
        let dots = module.chars().take_while(|ch| *ch == '.').count();
        let rest = module[dots..].replace('.', "/");
        let try_base = |base: &str| -> Vec<String> {
            let joined = if rest.is_empty() {
                Some(base.to_owned())
            } else {
                join_normalized(base, &rest)
            };
            let Some(joined) = joined else {
                return Vec::new();
            };
            if rest.is_empty() {
                vec![format!("{joined}/__init__.py")
                    .trim_start_matches('/')
                    .to_owned()]
            } else {
                vec![format!("{joined}.py"), format!("{joined}/__init__.py")]
            }
        };
        if dots > 0 {
            let mut base = dirname(ctx.path).to_owned();
            for _ in 1..dots {
                base = dirname(&base).to_owned();
            }
            return match try_base(&base)
                .into_iter()
                .find(|p| ctx.project.contains(p))
            {
                Some(path) => ModuleRef::File(path),
                None => ModuleRef::Unknown,
            };
        }
        for base in ["", dirname(ctx.path)] {
            if let Some(path) = try_base(base).into_iter().find(|p| ctx.project.contains(p)) {
                return ModuleRef::File(path);
            }
        }
        // Source-layout packages such as `src/pkg/mod.py` imported as `pkg.mod`.
        let suffixes = [format!("/{rest}.py"), format!("/{rest}/__init__.py")];
        let matches: Vec<&str> = ctx
            .project
            .paths()
            .filter(|path| {
                suffixes
                    .iter()
                    .any(|suffix| path.ends_with(suffix.as_str()))
            })
            .collect();
        match matches.as_slice() {
            [only] => ModuleRef::File((*only).to_owned()),
            [] => ModuleRef::External,
            _ => ModuleRef::Unknown,
        }
    }

    /// Finds `member` defined in `path`, following `from x import member` re-exports.
    fn follow(
        &self,
        ctx: &ResolutionContext<'_>,
        path: &str,
        member: &str,
        depth: usize,
        out: &mut Vec<String>,
    ) {
        if depth > 5 {
            return;
        }
        let Some(file) = ctx.project.file(path) else {
            return;
        };
        let defines = file.callables.iter().any(|c| {
            c.name == member
                && c.parent.is_none()
                && c.container.is_none()
                && matches!(c.kind, CallableKind::Function | CallableKind::Closure)
        }) || file.types.iter().any(|t| t.name == member);
        if defines {
            out.push(path.to_owned());
            return;
        }
        for import in file
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Named && i.local == member)
        {
            let inner = ResolutionContext {
                path,
                file,
                project: ctx.project,
                namespace: "",
            };
            if let ModuleRef::File(next) = self.module_ref(&inner, &import.module) {
                self.follow(ctx, &next, &import.imported, depth + 1, out);
            }
        }
    }
}

fn dotted(base: &str, rest: &[String]) -> String {
    let mut out = base.to_owned();
    for part in rest {
        if !out.ends_with('.') {
            out.push('.');
        }
        out.push_str(part);
    }
    out
}

impl Scheme for PythonScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Python
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
            .filter(|i| i.kind == ImportKind::Named && i.local == name)
        {
            let span = project_span(ctx.path, import.start, import.end);
            match self.module_ref(ctx, &import.module) {
                ModuleRef::File(path) => {
                    let mut found = Vec::new();
                    self.follow(ctx, &path, &import.imported, 0, &mut found);
                    for target in found {
                        lookup.hits.push(ImportHit {
                            path: target,
                            member: import.imported.clone(),
                            via_type: None,
                            namespace: None,
                            rule: "static-import",
                            span: span.clone(),
                        });
                    }
                }
                ModuleRef::External => lookup.external = Some((import.module.clone(), span)),
                ModuleRef::Unknown => {}
            }
        }
        if lookup.hits.is_empty() {
            for import in ctx
                .file
                .imports
                .iter()
                .filter(|i| i.kind == ImportKind::Wildcard)
            {
                let span = project_span(ctx.path, import.start, import.end);
                if let ModuleRef::File(path) = self.module_ref(ctx, &import.module) {
                    let mut found = Vec::new();
                    self.follow(ctx, &path, name, 0, &mut found);
                    for target in found {
                        lookup.hits.push(ImportHit {
                            path: target,
                            member: name.to_owned(),
                            via_type: None,
                            namespace: None,
                            rule: "wildcard-import",
                            span: span.clone(),
                        });
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
        let rest = &path[1..];
        for import in ctx.file.imports.iter().filter(|i| &i.local == first) {
            let base = match import.kind {
                ImportKind::Namespace if import.imported == "*" => import.module.clone(),
                ImportKind::Namespace => first.clone(),
                ImportKind::Named => dotted(&import.module, std::slice::from_ref(&import.imported)),
                _ => continue,
            };
            let target = dotted(&base, rest);
            let span = project_span(ctx.path, import.start, import.end);
            match self.module_ref(ctx, &target) {
                ModuleRef::File(file) => lookup.hits.push(ModuleHit {
                    path: file,
                    namespace: None,
                    rule: "module-import",
                    span,
                }),
                ModuleRef::External if import.kind == ImportKind::Namespace => {
                    lookup.external = Some((target, span));
                }
                _ => {}
            }
        }
        lookup
    }
}
