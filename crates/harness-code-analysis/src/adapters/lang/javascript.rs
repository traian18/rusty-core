//! JavaScript and TypeScript share one extractor; the profiles differ in grammar, type facts and
//! the declared limitations.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, ImportHit, ImportLookup, ModuleHit, ModuleLookup,
    ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Js {
    pub typescript: bool,
}

const CLOSURE_KINDS: [&str; 4] = [
    "arrow_function",
    "function_expression",
    "function",
    "generator_function",
];

impl Syntax for Js {
    fn language(&self) -> LanguageId {
        if self.typescript {
            LanguageId::TypeScript
        } else {
            LanguageId::JavaScript
        }
    }

    fn grammar(&self, path: &str) -> Language {
        if !self.typescript {
            tree_sitter_javascript::LANGUAGE.into()
        } else if path.ends_with(".tsx") {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    fn limitations(&self) -> Vec<String> {
        let mut limits = vec![
            "resolves relative ES imports/exports (named, default, namespace, re-exports) and CommonJS require of literal relative paths".into(),
            "bare-specifier imports are reported as external targets, not resolved into the workspace".into(),
            "dynamic import(), computed member calls, call/apply/bind, and prototype assignment stay unresolved".into(),
            "super(...) constructor chaining and tsconfig/package path aliases are not modelled".into(),
            "locals shadow definitions function-wide (block scopes are not distinguished)".into(),
        ];
        if self.typescript {
            limits.push("type-only imports never produce runtime calls; interface, generic, and union-typed receivers stay uncertain".into());
            limits.push(
                "explicit simple type annotations and `new` initializers type local receivers"
                    .into(),
            );
        } else {
            limits.push(
                "only `new` initializers type local receivers (no type annotations in JavaScript)"
                    .into(),
            );
        }
        limits
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "import_statement" => {
                self.import(node, c);
                Action::Skip
            }
            "export_statement" => {
                self.export(node, c);
                Action::Descend
            }
            "interface_declaration" | "type_alias_declaration" | "ambient_declaration" => {
                Action::Skip
            }
            "class_declaration" | "abstract_class_declaration" | "class" => {
                match node.child_by_field_name("name") {
                    Some(name) => {
                        let mut bases = Vec::new();
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            if child.kind() == "class_heritage" {
                                collect_names(child, c.src, &mut bases);
                            }
                        }
                        Action::Type(TypeSpec {
                            name: c.text(name).to_owned(),
                            name_range: (name.start_byte(), name.end_byte()),
                            kind: TypeKind::Class,
                            bases,
                        })
                    }
                    None => Action::Descend,
                }
            }
            "function_declaration" | "generator_function_declaration" => {
                let name = node.child_by_field_name("name");
                Action::Callable(CallableSpec {
                    name: name.map(|n| c.text(n).to_owned()),
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind: CallableKind::Function,
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            "method_definition" => {
                let name = node.child_by_field_name("name");
                let text = name.map(|n| c.text(n).to_owned());
                let kind = if text.as_deref() == Some("constructor") {
                    CallableKind::Constructor
                } else {
                    CallableKind::Method
                };
                let computed = name.is_some_and(|n| n.kind() == "computed_property_name");
                Action::Callable(CallableSpec {
                    name: if computed {
                        Some("<computed>".into())
                    } else {
                        text
                    },
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind,
                    container: None,
                    namespace: None,
                    params: params(node, c),
                })
            }
            kind if CLOSURE_KINDS.contains(&kind) => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: params(node, c),
            }),
            "public_field_definition" | "field_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("property"));
                if let (Some(name), Some(value)) = (name, node.child_by_field_name("value")) {
                    if CLOSURE_KINDS.contains(&value.kind()) {
                        c.name_closure_as(value, c.text(name), CallableKind::Method);
                    }
                }
                Action::Descend
            }
            "variable_declarator" => self.declarator(node, c),
            "catch_clause" => {
                if let Some(parameter) = node.child_by_field_name("parameter") {
                    let mut names = Vec::new();
                    binding_names(parameter, c.src, &mut names);
                    for name in names {
                        c.declare(&name, true);
                    }
                }
                Action::Descend
            }
            "call_expression" => {
                self.call(node, c);
                Action::Descend
            }
            "new_expression" => {
                self.construct(node, c);
                Action::Descend
            }
            _ => Action::Descend,
        }
    }
}

fn collect_names(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "type_identifier" => out.push(src[node.byte_range()].to_owned()),
        "member_expression" | "nested_type_identifier" => {
            if let Some(name) = node
                .child_by_field_name("property")
                .or_else(|| node.child_by_field_name("name"))
            {
                out.push(src[name.byte_range()].to_owned());
            }
        }
        "type_arguments" | "arguments" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_names(child, src, out);
            }
        }
    }
}

fn ts_type(node: Node<'_>, src: &str) -> Option<String> {
    let text = src[node.byte_range()].trim_start_matches(':').trim();
    simple_type(text)
}

fn params(node: Node<'_>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(parameter) = node.child_by_field_name("parameter") {
        let mut names = Vec::new();
        binding_names(parameter, c.src, &mut names);
        out.extend(names.into_iter().map(|name| (name, None)));
    }
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = parameters.walk();
    for child in parameters.named_children(&mut cursor) {
        let mut names = Vec::new();
        match child.kind() {
            "required_parameter" | "optional_parameter" => {
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    binding_names(pattern, c.src, &mut names);
                }
                let ty = child
                    .child_by_field_name("type")
                    .and_then(|t| ts_type(t, c.src));
                out.extend(names.into_iter().map(|name| (name, ty.clone())));
            }
            _ => {
                binding_names(child, c.src, &mut names);
                out.extend(names.into_iter().map(|name| (name, None)));
            }
        }
    }
    out
}

impl Js {
    fn declarator(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        let Some(name) = node.child_by_field_name("name") else {
            return Action::Descend;
        };
        let value = node.child_by_field_name("value");
        if let Some(value) = value {
            if self.require(name, value, c) {
                return Action::Skip;
            }
        }
        if name.kind() == "identifier" {
            let text = c.text(name).to_owned();
            let (ty, exact) = match value {
                Some(value) if value.kind() == "new_expression" => (
                    value
                        .child_by_field_name("constructor")
                        .and_then(|ctor| path_segments(ctor, c.src))
                        .and_then(|segments| segments.last().cloned()),
                    true,
                ),
                _ => (
                    node.child_by_field_name("type")
                        .and_then(|t| ts_type(t, c.src)),
                    false,
                ),
            };
            c.declare_typed(&text, ty, exact, false);
            if let Some(value) = value {
                if CLOSURE_KINDS.contains(&value.kind()) {
                    c.name_closure(value, &text);
                }
            }
        } else {
            let mut names = Vec::new();
            binding_names(name, c.src, &mut names);
            for name in names {
                c.declare(&name, false);
            }
        }
        Action::Descend
    }

    /// `const x = require('./m')` / `const {a, b: c} = require('./m')`.
    fn require(&self, name: Node<'_>, value: Node<'_>, c: &mut Collector<'_>) -> bool {
        if value.kind() != "call_expression" {
            return false;
        }
        let Some(function) = value.child_by_field_name("function") else {
            return false;
        };
        if c.text(function) != "require" {
            return false;
        }
        let Some(arguments) = value.child_by_field_name("arguments") else {
            return false;
        };
        let Some(first) = arguments.named_child(0) else {
            return false;
        };
        if first.kind() != "string" {
            return false;
        }
        let module = unquote(c.text(first)).to_owned();
        let (start, end) = (value.start_byte() as u32, value.end_byte() as u32);
        match name.kind() {
            "identifier" => c.import(ImportFact {
                local: c.text(name).to_owned(),
                imported: "*".into(),
                module,
                kind: ImportKind::Namespace,
                type_only: false,
                start,
                end,
            }),
            "object_pattern" => {
                let mut cursor = name.walk();
                for child in name.named_children(&mut cursor) {
                    let (imported, local) = match child.kind() {
                        "shorthand_property_identifier_pattern" => {
                            (c.text(child).to_owned(), c.text(child).to_owned())
                        }
                        "pair_pattern" => {
                            let key = child.child_by_field_name("key");
                            let value = child.child_by_field_name("value");
                            match (key, value) {
                                (Some(key), Some(value)) if value.kind() == "identifier" => {
                                    (c.text(key).to_owned(), c.text(value).to_owned())
                                }
                                _ => continue,
                            }
                        }
                        _ => continue,
                    };
                    c.import(ImportFact {
                        local,
                        imported,
                        module: module.clone(),
                        kind: ImportKind::Named,
                        type_only: false,
                        start,
                        end,
                    });
                }
            }
            _ => return false,
        }
        true
    }

    fn import(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(source) = node.child_by_field_name("source") else {
            return;
        };
        let module = unquote(c.text(source)).to_owned();
        let statement_type_only = c.text(node).trim_start().starts_with("import type ");
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let mut cursor = node.walk();
        for clause in node.children(&mut cursor) {
            if clause.kind() != "import_clause" {
                continue;
            }
            let mut inner = clause.walk();
            for part in clause.children(&mut inner) {
                match part.kind() {
                    "identifier" => c.import(ImportFact {
                        local: c.text(part).to_owned(),
                        imported: "default".into(),
                        module: module.clone(),
                        kind: ImportKind::Default,
                        type_only: statement_type_only,
                        start,
                        end,
                    }),
                    "namespace_import" => {
                        let mut ns = part.walk();
                        let local = part
                            .children(&mut ns)
                            .find(|n| n.kind() == "identifier")
                            .map(|n| c.text(n).to_owned());
                        if let Some(local) = local {
                            c.import(ImportFact {
                                local,
                                imported: "*".into(),
                                module: module.clone(),
                                kind: ImportKind::Namespace,
                                type_only: statement_type_only,
                                start,
                                end,
                            });
                        }
                    }
                    "named_imports" => {
                        let mut specs = part.walk();
                        for spec in part.named_children(&mut specs) {
                            if spec.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(name) = spec.child_by_field_name("name") else {
                                continue;
                            };
                            let imported = c.text(name).to_owned();
                            let local = spec
                                .child_by_field_name("alias")
                                .map_or_else(|| imported.clone(), |a| c.text(a).to_owned());
                            let type_only = statement_type_only
                                || c.text(spec).trim_start().starts_with("type ");
                            c.import(ImportFact {
                                local,
                                imported,
                                module: module.clone(),
                                kind: ImportKind::Named,
                                type_only,
                                start,
                                end,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn export(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let from = node
            .child_by_field_name("source")
            .map(|source| unquote(c.text(source)).to_owned());
        let mut cursor = node.walk();
        let is_default = node
            .children(&mut cursor)
            .any(|child| child.kind() == "default");
        let mut cursor = node.walk();
        let has_clause = node
            .children(&mut cursor)
            .any(|child| child.kind() == "export_clause");
        if let Some(declaration) = node.child_by_field_name("declaration") {
            let mut names = Vec::new();
            match declaration.kind() {
                "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration" => {
                    if let Some(name) = declaration.child_by_field_name("name") {
                        names.push(c.text(name).to_owned());
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    let mut decls = declaration.walk();
                    for declarator in declaration.named_children(&mut decls) {
                        if let Some(name) = declarator.child_by_field_name("name") {
                            binding_names(name, c.src, &mut names);
                        }
                    }
                }
                _ => {}
            }
            if names.is_empty() && is_default {
                c.export(ExportFact {
                    exported: "default".into(),
                    local: String::new(),
                    from: None,
                    start,
                    end,
                });
            }
            for name in names {
                c.export(ExportFact {
                    exported: if is_default {
                        "default".into()
                    } else {
                        name.clone()
                    },
                    local: name,
                    from: None,
                    start,
                    end,
                });
            }
        } else if let Some(value) = node.child_by_field_name("value") {
            let local = if value.kind() == "identifier" {
                c.text(value).to_owned()
            } else {
                String::new()
            };
            c.export(ExportFact {
                exported: "default".into(),
                local,
                from: None,
                start,
                end,
            });
        } else if has_clause {
            let mut cursor = node.walk();
            let clause = node
                .children(&mut cursor)
                .find(|child| child.kind() == "export_clause");
            if let Some(clause) = clause {
                let mut specs = clause.walk();
                for spec in clause.named_children(&mut specs) {
                    let Some(name) = spec.child_by_field_name("name") else {
                        continue;
                    };
                    let local = c.text(name).to_owned();
                    let exported = spec
                        .child_by_field_name("alias")
                        .map_or_else(|| local.clone(), |a| c.text(a).to_owned());
                    c.export(ExportFact {
                        exported,
                        local,
                        from: from.clone(),
                        start,
                        end,
                    });
                }
            }
        } else if from.is_some() {
            c.export(ExportFact {
                exported: "*".into(),
                local: String::new(),
                from,
                start,
                end,
            });
        }
    }

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        match function.kind() {
            "identifier" => {
                if c.text(function) == "require" {
                    return;
                }
                c.call(CallSpec {
                    name: c.text(function).to_owned(),
                    kind: CallKind::Function,
                    receiver: Receiver::None,
                    range: (function.start_byte(), function.end_byte()),
                });
            }
            "member_expression" => {
                let property = function.child_by_field_name("property");
                let object = function.child_by_field_name("object");
                match (object, property) {
                    (Some(object), Some(property))
                        if matches!(
                            property.kind(),
                            "property_identifier" | "private_property_identifier"
                        ) =>
                    {
                        c.call(CallSpec {
                            name: c.text(property).to_owned(),
                            kind: CallKind::Method,
                            receiver: receiver_from(object, c.src),
                            range: (property.start_byte(), property.end_byte()),
                        });
                    }
                    _ => dynamic_call(c, function),
                }
            }
            "super" => {}
            _ => dynamic_call(c, function),
        }
    }

    fn construct(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(constructor) = node.child_by_field_name("constructor") else {
            return;
        };
        match constructor.kind() {
            "identifier" => c.call(CallSpec {
                name: c.text(constructor).to_owned(),
                kind: CallKind::Constructor,
                receiver: Receiver::None,
                range: (constructor.start_byte(), constructor.end_byte()),
            }),
            "member_expression" => {
                let property = constructor.child_by_field_name("property");
                let object = constructor.child_by_field_name("object");
                match (object, property) {
                    (Some(object), Some(property)) => c.call(CallSpec {
                        name: c.text(property).to_owned(),
                        kind: CallKind::Constructor,
                        receiver: receiver_from(object, c.src),
                        range: (property.start_byte(), property.end_byte()),
                    }),
                    _ => dynamic_call(c, constructor),
                }
            }
            _ => dynamic_call(c, constructor),
        }
    }
}

// ----- resolution -----

pub(crate) struct JsScheme {
    pub typescript: bool,
}

enum ModuleRef {
    File(String),
    External,
    Unknown,
}

const EXTENSIONS: [&str; 8] = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];

impl JsScheme {
    fn module_ref(&self, ctx: &ResolutionContext<'_>, from: &str, module: &str) -> ModuleRef {
        if !module.starts_with("./")
            && !module.starts_with("../")
            && module != "."
            && module != ".."
        {
            return ModuleRef::External;
        }
        let Some(base) = join_normalized(dirname(from), module) else {
            return ModuleRef::Unknown;
        };
        let mut candidates = Vec::new();
        candidates.push(base.clone());
        let stem = ["js", "jsx", "mjs", "cjs"]
            .iter()
            .find_map(|ext| base.strip_suffix(&format!(".{ext}")));
        let order: Vec<&str> = if self.typescript {
            EXTENSIONS.to_vec()
        } else {
            vec![".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx"]
        };
        if let Some(stem) = stem {
            for ext in &order {
                candidates.push(format!("{stem}{ext}"));
            }
        }
        for ext in &order {
            candidates.push(format!("{base}{ext}"));
        }
        for ext in &order {
            candidates.push(format!("{base}/index{ext}"));
        }
        match candidates
            .into_iter()
            .find(|path| ctx.project.contains(path))
        {
            Some(path) => ModuleRef::File(path),
            None => ModuleRef::Unknown,
        }
    }

    /// Follows exports (including re-exports) to the declaring file and local name.
    fn export_targets(
        &self,
        ctx: &ResolutionContext<'_>,
        file_path: &str,
        exported: &str,
        depth: usize,
        out: &mut Vec<(String, String)>,
    ) {
        if depth > 5 {
            return;
        }
        let Some(file) = ctx.project.file(file_path) else {
            return;
        };
        for export in &file.exports {
            if export.exported == exported {
                match &export.from {
                    Some(module) => {
                        if let ModuleRef::File(next) = self.module_ref(ctx, file_path, module) {
                            let name = if export.local.is_empty() {
                                exported
                            } else {
                                &export.local
                            };
                            self.export_targets(ctx, &next, name, depth + 1, out);
                        }
                    }
                    None if !export.local.is_empty() => {
                        out.push((file_path.to_owned(), export.local.clone()));
                    }
                    None => {}
                }
            } else if export.exported == "*" && exported != "default" {
                if let Some(module) = &export.from {
                    if let ModuleRef::File(next) = self.module_ref(ctx, file_path, module) {
                        self.export_targets(ctx, &next, exported, depth + 1, out);
                    }
                }
            }
        }
    }
}

impl Scheme for JsScheme {
    fn language(&self) -> LanguageId {
        if self.typescript {
            LanguageId::TypeScript
        } else {
            LanguageId::JavaScript
        }
    }

    fn named_import(&self, ctx: &ResolutionContext<'_>, name: &str) -> ImportLookup {
        let mut lookup = ImportLookup::default();
        for import in ctx.file.imports.iter().filter(|import| {
            import.local == name
                && matches!(import.kind, ImportKind::Named | ImportKind::Default)
                && !import.type_only
        }) {
            let span = project_span(ctx.path, import.start, import.end);
            match self.module_ref(ctx, ctx.path, &import.module) {
                ModuleRef::File(path) => {
                    let wanted = if import.kind == ImportKind::Default {
                        "default"
                    } else {
                        import.imported.as_str()
                    };
                    let mut targets = Vec::new();
                    self.export_targets(ctx, &path, wanted, 0, &mut targets);
                    for (target_path, member) in targets {
                        lookup.hits.push(ImportHit {
                            path: target_path,
                            member,
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
        lookup
    }

    fn module_alias(&self, ctx: &ResolutionContext<'_>, path: &[String]) -> ModuleLookup {
        let mut lookup = ModuleLookup::default();
        let [name] = path else {
            return lookup;
        };
        for import in ctx.file.imports.iter().filter(|import| {
            &import.local == name && import.kind == ImportKind::Namespace && !import.type_only
        }) {
            let span = project_span(ctx.path, import.start, import.end);
            match self.module_ref(ctx, ctx.path, &import.module) {
                ModuleRef::File(file) => lookup.hits.push(ModuleHit {
                    path: file,
                    namespace: None,
                    rule: "namespace-import",
                    span,
                }),
                ModuleRef::External => lookup.external = Some((import.module.clone(), span)),
                ModuleRef::Unknown => {}
            }
        }
        lookup
    }
}
