use std::path::Path;
use std::sync::Arc;

use tree_sitter::{Language, Node, Parser};

use crate::api::CallGraphError;
use crate::model::{Diagnostic, LanguageCapabilities, LanguageId};

#[derive(Clone, Debug)]
pub struct CallableFact {
    pub name: String,
    pub owner: Option<String>,
    pub start: u32,
    pub end: u32,
    pub body_start: u32,
    pub body_end: u32,
}

#[derive(Clone, Debug)]
pub struct CallFact {
    pub caller: Option<String>,
    pub name: String,
    pub qualifier: Option<String>,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub struct ImportFact {
    pub local: String,
    pub imported: String,
    pub module: String,
}

#[derive(Clone, Debug)]
pub struct ExtractedFile {
    pub language: LanguageId,
    pub callables: Vec<CallableFact>,
    pub calls: Vec<CallFact>,
    pub imports: Vec<ImportFact>,
    pub diagnostics: Vec<Diagnostic>,
}

pub trait LanguageAdapter: Send + Sync {
    fn language(&self) -> LanguageId;
    fn version(&self) -> &'static str;
    fn capabilities(&self) -> LanguageCapabilities;
    fn extract(&self, path: &str, source: &str) -> Result<ExtractedFile, CallGraphError>;
}

pub trait LanguageResolver: Send + Sync {
    fn imported_name<'a>(&self, file: &'a ExtractedFile, local: &str) -> Option<&'a ImportFact>;
}

pub(crate) struct AdapterRegistry {
    adapters: Vec<Arc<dyn LanguageAdapter>>,
    resolver: Arc<dyn LanguageResolver>,
}

pub struct StaticResolver;

impl AdapterRegistry {
    pub(crate) fn new(
        adapters: Vec<Arc<dyn LanguageAdapter>>,
        resolver: Arc<dyn LanguageResolver>,
    ) -> Result<Self, CallGraphError> {
        if adapters.is_empty() {
            return Err(CallGraphError::InvalidRequest(
                "at least one adapter is required".into(),
            ));
        }
        for adapter in &adapters {
            let language = adapter.language();
            if adapters
                .iter()
                .filter(|candidate| candidate.language() == language)
                .count()
                > 1
            {
                return Err(CallGraphError::InvalidRequest(
                    "duplicate language adapter".into(),
                ));
            }
        }
        Ok(Self { adapters, resolver })
    }

    pub(crate) fn builtin() -> Self {
        let languages = [
            LanguageId::JavaScript,
            LanguageId::TypeScript,
            LanguageId::Python,
            LanguageId::Java,
            LanguageId::CSharp,
            LanguageId::Go,
            LanguageId::Rust,
            LanguageId::Php,
            LanguageId::Cpp,
            LanguageId::Ruby,
        ];
        Self {
            adapters: languages
                .into_iter()
                .map(|language| {
                    Arc::new(TreeSitterAdapter { language }) as Arc<dyn LanguageAdapter>
                })
                .collect(),
            resolver: Arc::new(StaticResolver),
        }
    }

    pub(crate) fn adapter_for_path(&self, path: &str) -> Option<&dyn LanguageAdapter> {
        let language = language_for(path)?;
        self.adapters
            .iter()
            .find(|adapter| adapter.language() == language)
            .map(AsRef::as_ref)
    }

    pub(crate) fn capabilities(&self) -> Vec<LanguageCapabilities> {
        self.adapters
            .iter()
            .map(|adapter| adapter.capabilities())
            .collect()
    }

    pub(crate) fn resolver(&self) -> &dyn LanguageResolver {
        self.resolver.as_ref()
    }
}

impl LanguageResolver for StaticResolver {
    fn imported_name<'a>(&self, file: &'a ExtractedFile, local: &str) -> Option<&'a ImportFact> {
        file.imports.iter().find(|item| item.local == local)
    }
}

struct TreeSitterAdapter {
    language: LanguageId,
}

impl LanguageAdapter for TreeSitterAdapter {
    fn language(&self) -> LanguageId {
        self.language.clone()
    }

    fn version(&self) -> &'static str {
        "tree-sitter-1"
    }

    fn capabilities(&self) -> LanguageCapabilities {
        LanguageCapabilities {
            language: self.language(),
            supported: true,
            limitations: vec![
                "syntax-tree extraction with bounded static name and import resolution".into(),
                "dynamic dispatch, overloads, macros, generated code, and runtime imports remain unresolved".into(),
            ],
        }
    }

    fn extract(&self, path: &str, source: &str) -> Result<ExtractedFile, CallGraphError> {
        let mut parser = Parser::new();
        parser
            .set_language(&grammar(&self.language, path))
            .map_err(|error| CallGraphError::DependencyFailure(error.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| CallGraphError::DependencyFailure("parser cancelled".into()))?;
        let mut callables = Vec::new();
        let mut calls = Vec::new();
        walk(
            tree.root_node(),
            source,
            &self.language,
            None,
            &mut callables,
            &mut calls,
        );
        assign_callers(&callables, &mut calls);
        let mut diagnostics = Vec::new();
        collect_errors(tree.root_node(), path, &mut diagnostics);
        Ok(ExtractedFile {
            language: self.language(),
            callables,
            calls,
            imports: imports(source, &self.language),
            diagnostics,
        })
    }
}

fn grammar(language: &LanguageId, path: &str) -> Language {
    match language {
        LanguageId::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        LanguageId::TypeScript if path.ends_with(".tsx") => {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        }
        LanguageId::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        LanguageId::Python => tree_sitter_python::LANGUAGE.into(),
        LanguageId::Java => tree_sitter_java::LANGUAGE.into(),
        LanguageId::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        LanguageId::Go => tree_sitter_go::LANGUAGE.into(),
        LanguageId::Rust => tree_sitter_rust::LANGUAGE.into(),
        LanguageId::Php => tree_sitter_php::LANGUAGE_PHP_ONLY.into(),
        LanguageId::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        LanguageId::Ruby => tree_sitter_ruby::LANGUAGE.into(),
    }
}

fn walk(
    node: Node<'_>,
    source: &str,
    language: &LanguageId,
    owner: Option<&str>,
    callables: &mut Vec<CallableFact>,
    calls: &mut Vec<CallFact>,
) {
    let next_owner = type_name(node, source, language).or_else(|| owner.map(str::to_owned));
    if is_callable(node.kind(), language) {
        let name_node = callable_name_node(node, language);
        let (name, start, end) = name_node.map_or_else(
            || {
                let start = node.start_byte();
                (format!("<closure@{start}>"), start, start)
            },
            |name| {
                (
                    text(name, source).to_owned(),
                    name.start_byte(),
                    name.end_byte(),
                )
            },
        );
        let body = node.child_by_field_name("body").unwrap_or(node);
        callables.push(CallableFact {
            name: name.clone(),
            owner: next_owner.clone(),
            start: start as u32,
            end: end as u32,
            body_start: body.start_byte() as u32,
            body_end: body.end_byte() as u32,
        });
    }
    if is_call(node.kind(), language) {
        if let Some((name, qualifier, start, end)) = call_name(node, source, language) {
            calls.push(CallFact {
                caller: None,
                name,
                qualifier,
                start: start as u32,
                end: end as u32,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(
            child,
            source,
            language,
            next_owner.as_deref(),
            callables,
            calls,
        );
    }
}

fn assign_callers(callables: &[CallableFact], calls: &mut [CallFact]) {
    for call in calls {
        call.caller = callables
            .iter()
            .filter(|item| call.start >= item.body_start && call.start < item.body_end)
            .min_by_key(|item| item.body_end - item.body_start)
            .map(|item| item.name.clone());
    }
}

fn is_callable(kind: &str, language: &LanguageId) -> bool {
    match language {
        LanguageId::JavaScript | LanguageId::TypeScript => matches!(
            kind,
            "function_declaration" | "method_definition" | "arrow_function" | "function_expression"
        ),
        LanguageId::Python => matches!(kind, "function_definition" | "lambda"),
        LanguageId::Java => matches!(
            kind,
            "method_declaration" | "constructor_declaration" | "lambda_expression"
        ),
        LanguageId::CSharp => matches!(
            kind,
            "method_declaration"
                | "constructor_declaration"
                | "local_function_statement"
                | "lambda_expression"
        ),
        LanguageId::Go => matches!(
            kind,
            "function_declaration" | "method_declaration" | "func_literal"
        ),
        LanguageId::Rust => matches!(kind, "function_item" | "closure_expression"),
        LanguageId::Php => matches!(
            kind,
            "function_definition" | "method_declaration" | "anonymous_function" | "arrow_function"
        ),
        LanguageId::Cpp => matches!(kind, "function_definition" | "lambda_expression"),
        LanguageId::Ruby => matches!(kind, "method" | "singleton_method" | "lambda" | "block"),
    }
}

fn is_call(kind: &str, language: &LanguageId) -> bool {
    match language {
        LanguageId::Python => kind == "call",
        LanguageId::Java => matches!(kind, "method_invocation" | "object_creation_expression"),
        LanguageId::CSharp => {
            matches!(kind, "invocation_expression" | "object_creation_expression")
        }
        LanguageId::Php => matches!(
            kind,
            "function_call_expression"
                | "member_call_expression"
                | "scoped_call_expression"
                | "object_creation_expression"
        ),
        LanguageId::Ruby => matches!(kind, "call" | "method_call" | "command"),
        _ => kind == "call_expression",
    }
}

fn callable_name_node<'a>(node: Node<'a>, language: &LanguageId) -> Option<Node<'a>> {
    node.child_by_field_name("name").or_else(|| {
        if matches!(language, LanguageId::Cpp) {
            node.child_by_field_name("declarator")
                .and_then(find_identifier)
        } else {
            None
        }
    })
}

fn type_name(node: Node<'_>, source: &str, language: &LanguageId) -> Option<String> {
    let is_type = match language {
        LanguageId::Rust => node.kind() == "impl_item",
        LanguageId::Go => false,
        _ => matches!(
            node.kind(),
            "class_declaration"
                | "class_definition"
                | "interface_declaration"
                | "struct_specifier"
                | "module"
        ),
    };
    if !is_type {
        return None;
    }
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("type"))
        .map(|name| text(name, source).to_owned())
}

fn call_name(
    node: Node<'_>,
    source: &str,
    _language: &LanguageId,
) -> Option<(String, Option<String>, usize, usize)> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("type"))?;
    let leaf = find_last_identifier(function)?;
    let qualifier = function
        .child_by_field_name("object")
        .or_else(|| function.child_by_field_name("scope"))
        .map(|item| text(item, source).to_owned())
        .or_else(|| {
            let value = text(function, source);
            value
                .rsplit_once(['.', ':'])
                .map(|(prefix, _)| prefix.trim_end_matches(':').to_owned())
        });
    let name = text(leaf, source).to_owned();
    Some((name, qualifier, leaf.start_byte(), leaf.end_byte()))
}

fn find_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "constant"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find_map(find_identifier);
    found
}

fn find_last_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "constant" | "method"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter_map(find_last_identifier)
        .last()
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn collect_errors(node: Node<'_>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if node.is_error() || node.is_missing() {
        diagnostics.push(Diagnostic {
            path: Some(path.to_owned()),
            message: format!(
                "syntax error at bytes {}..{}; extraction may be incomplete",
                node.start_byte(),
                node.end_byte()
            ),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, path, diagnostics);
    }
}

fn imports(source: &str, language: &LanguageId) -> Vec<ImportFact> {
    source
        .lines()
        .filter_map(|line| parse_import(line.trim(), language))
        .collect()
}

fn parse_import(line: &str, language: &LanguageId) -> Option<ImportFact> {
    match language {
        LanguageId::Python => {
            let rest = line.strip_prefix("from ")?;
            let (module, names) = rest.split_once(" import ")?;
            let (imported, local) = alias(names.trim());
            Some(import(module, imported, local))
        }
        LanguageId::JavaScript | LanguageId::TypeScript => {
            let rest = line
                .strip_prefix("import ")?
                .strip_prefix("type ")
                .unwrap_or_else(|| line.strip_prefix("import ").unwrap());
            let (names, module) = rest.split_once(" from ")?;
            let imported = names.trim().trim_matches(|c| c == '{' || c == '}');
            let (imported, local) = alias(imported);
            Some(import(
                module.trim_matches(|c| c == '\'' || c == '"' || c == ';'),
                imported,
                local,
            ))
        }
        LanguageId::Rust => {
            let rest = line.strip_prefix("use ")?.trim_end_matches(';');
            let imported = rest.rsplit("::").next()?;
            let (imported, local) = alias(imported);
            Some(import(rest, imported, local))
        }
        LanguageId::Java | LanguageId::CSharp | LanguageId::Php | LanguageId::Cpp => {
            let rest = line
                .strip_prefix("import ")
                .or_else(|| line.strip_prefix("using "))?;
            let module = rest.trim_end_matches(';');
            let imported = module.rsplit(['.', '\\']).next()?;
            Some(import(module, imported, imported))
        }
        LanguageId::Go => {
            let module = line.strip_prefix("import ")?.trim_matches('"');
            let local = module.rsplit('/').next()?;
            Some(import(module, local, local))
        }
        LanguageId::Ruby => {
            let module = line
                .strip_prefix("require_relative ")?
                .trim_matches(|c| c == '\'' || c == '"');
            let local = module.rsplit('/').next()?;
            Some(import(module, local, local))
        }
    }
}

fn import(module: &str, imported: &str, local: &str) -> ImportFact {
    ImportFact {
        local: local.into(),
        imported: imported.into(),
        module: module.into(),
    }
}

fn alias(value: &str) -> (&str, &str) {
    value
        .split_once(" as ")
        .map_or((value, value), |(name, alias)| (name.trim(), alias.trim()))
}

fn language_for(path: &str) -> Option<LanguageId> {
    Some(match Path::new(path).extension()?.to_str()? {
        "js" | "jsx" => LanguageId::JavaScript,
        "ts" | "tsx" => LanguageId::TypeScript,
        "py" => LanguageId::Python,
        "java" => LanguageId::Java,
        "cs" => LanguageId::CSharp,
        "go" => LanguageId::Go,
        "rs" => LanguageId::Rust,
        "php" => LanguageId::Php,
        "c" | "cc" | "cpp" | "h" | "hpp" => LanguageId::Cpp,
        "rb" => LanguageId::Ruby,
        _ => return None,
    })
}
