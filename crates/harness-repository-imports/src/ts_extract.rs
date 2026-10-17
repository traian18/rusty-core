//! TypeScript/JavaScript import extraction: static imports, re-exports, `require`, and `import()`.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, ImportFact, ImportFactId, ImportForm, ImportKind,
        ImportLimits, ItemKind, LanguageImportAdapter, LanguageProfile, PathAttribute, Staticness,
        Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::{Language, Node, Parser};

pub const TS_ADAPTER_VERSION: u32 = 1;
const MAX_SPELLING: usize = 200;

#[derive(Clone, Copy, Debug, Default)]
pub struct TypeScriptImportAdapter;

fn extension(path: &RootRelativePath) -> Option<&str> {
    path.as_path().extension().and_then(|e| e.to_str())
}

impl LanguageImportAdapter for TypeScriptImportAdapter {
    fn id(&self) -> &'static str {
        "typescript-javascript-imports"
    }

    fn version(&self) -> u32 {
        TS_ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        match extension(path)? {
            "ts" | "tsx" | "mts" | "cts" => Some(LanguageProfile::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(LanguageProfile::JavaScript),
            _ => None,
        }
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![
            ImportForm::Import,
            ImportForm::ExportFrom,
            ImportForm::ExportAllFrom,
            ImportForm::Require,
            ImportForm::DynamicImport,
        ]
    }

    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        let mut out = ExtractedFile::default();
        let language: Language = match (profile, extension(path)) {
            (LanguageProfile::JavaScript, _) => tree_sitter_javascript::LANGUAGE.into(),
            (_, Some("tsx")) => tree_sitter_typescript::LANGUAGE_TSX.into(),
            _ => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        };
        let mut parser = Parser::new();
        if parser.set_language(&language).is_err() {
            out.diagnostics.push("grammar is unavailable".into());
            return out;
        }
        let Some(tree) = parser.parse(source, None) else {
            out.diagnostics.push("parse produced no tree".into());
            return out;
        };
        if tree.root_node().has_error() {
            out.diagnostics
                .push("source has syntax errors; extraction may be incomplete".into());
        }
        let mut walker = Walker {
            path,
            profile,
            source,
            limits,
            out,
        };
        walker.walk(tree.root_node());
        walker.out
    }
}

struct Walker<'a> {
    path: &'a RootRelativePath,
    profile: LanguageProfile,
    source: &'a str,
    limits: &'a ImportLimits,
    out: ExtractedFile,
}

fn span(node: Node) -> ByteSpan {
    ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

impl<'a> Walker<'a> {
    fn text(&self, node: Node) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn walk(&mut self, root: Node) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "import_statement" => self.import_statement(node),
                "export_statement" => self.export_statement(node),
                "call_expression" => self.call_expression(node),
                _ => {}
            }
            let mut cursor = node.walk();
            let children = node.children(&mut cursor).collect::<Vec<_>>();
            stack.extend(children.into_iter().rev());
        }
    }

    fn push(&mut self, fact: ImportFact) {
        if self.out.facts.len() >= self.limits.max_facts_per_file {
            self.out.truncated = true;
        } else {
            self.out.facts.push(fact);
        }
    }

    fn fact(&self, node: Node, spelling: String, form: ImportForm) -> ImportFact {
        let span = span(node);
        ImportFact {
            id: ImportFactId(format!(
                "{}:{}:{}",
                self.path.as_path().display(),
                span.start,
                span.end
            )),
            importing_file: self.path.clone(),
            spelling,
            form,
            span,
            profile: self.profile,
            staticness: Staticness::Static,
            kind: ImportKind::Runtime,
            scope: Vec::new(),
            imported_name: None,
            alias: None,
            glob: false,
            reexport: false,
            literal_specifier: true,
            path_attribute: PathAttribute::None,
        }
    }

    /// The module specifier without quotes, or `None` for anything but a plain string.
    fn literal(&self, node: Node) -> Option<String> {
        if node.kind() != "string" {
            return None;
        }
        let text = self.text(node);
        let inner = text.get(1..text.len().checked_sub(1)?)?;
        Some(inner.to_owned())
    }

    fn has_keyword(&self, node: Node, keyword: &str) -> bool {
        let mut cursor = node.walk();
        let found = node
            .children(&mut cursor)
            .take_while(|c| !matches!(c.kind(), "import_clause" | "export_clause" | "string" | "*"))
            .any(|c| !c.is_named() && c.kind() == keyword);
        found
    }

    fn import_statement(&mut self, node: Node) {
        let Some(source) = node.child_by_field_name("source") else {
            // `import x = require("y")`
            let mut cursor = node.walk();
            let require = node
                .children(&mut cursor)
                .find(|c| c.kind() == "import_require_clause");
            if let Some(clause) = require {
                if let Some(literal) = clause
                    .child_by_field_name("source")
                    .and_then(|s| self.literal(s))
                {
                    let mut fact = self.fact(node, literal, ImportForm::Require);
                    fact.imported_name = Some("*".into());
                    self.push(fact);
                }
            }
            return;
        };
        let Some(spelling) = self.literal(source) else {
            return;
        };
        let statement_type = self.has_keyword(node, "type");
        let mut cursor = node.walk();
        let clause = node
            .children(&mut cursor)
            .find(|c| c.kind() == "import_clause");
        let Some(clause) = clause else {
            let fact = self.fact(node, spelling, ImportForm::Import);
            self.push(fact);
            return;
        };
        let mut produced = false;
        let mut cursor = clause.walk();
        for part in clause.children(&mut cursor) {
            match part.kind() {
                "identifier" => {
                    let mut fact = self.fact(part, spelling.clone(), ImportForm::Import);
                    fact.imported_name = Some("default".into());
                    fact.alias = Some(self.text(part).to_owned());
                    fact.kind = kind(statement_type);
                    self.push(fact);
                    produced = true;
                }
                "namespace_import" => {
                    let mut fact = self.fact(part, spelling.clone(), ImportForm::Import);
                    fact.imported_name = Some("*".into());
                    fact.alias = self.last_identifier(part);
                    fact.kind = kind(statement_type);
                    self.push(fact);
                    produced = true;
                }
                "named_imports" => {
                    let mut inner = part.walk();
                    for specifier in part
                        .children(&mut inner)
                        .filter(|c| c.kind() == "import_specifier")
                    {
                        let mut fact = self.fact(specifier, spelling.clone(), ImportForm::Import);
                        fact.imported_name = specifier
                            .child_by_field_name("name")
                            .map(|n| self.text(n).to_owned());
                        fact.alias = specifier
                            .child_by_field_name("alias")
                            .map(|n| self.text(n).to_owned());
                        fact.kind = kind(statement_type || self.has_keyword_in(specifier));
                        self.push(fact);
                        produced = true;
                    }
                }
                _ => {}
            }
        }
        if !produced {
            let fact = self.fact(node, spelling, ImportForm::Import);
            self.push(fact);
        }
    }

    fn has_keyword_in(&self, specifier: Node) -> bool {
        let mut cursor = specifier.walk();
        let found = specifier
            .children(&mut cursor)
            .any(|c| !c.is_named() && c.kind() == "type");
        found
    }

    fn last_identifier(&self, node: Node) -> Option<String> {
        let mut cursor = node.walk();
        let name = node
            .children(&mut cursor)
            .filter(|c| c.kind() == "identifier")
            .last()
            .map(|c| self.text(c).to_owned());
        name
    }

    fn export_statement(&mut self, node: Node) {
        if let Some(declaration) = node.child_by_field_name("declaration") {
            self.declaration_items(declaration);
        }
        let mut cursor = node.walk();
        let children = node.children(&mut cursor).collect::<Vec<_>>();
        let source = node
            .child_by_field_name("source")
            .and_then(|s| self.literal(s));
        let statement_type = self.has_keyword(node, "type");
        for child in &children {
            match child.kind() {
                "export_clause" => {
                    let mut inner = child.walk();
                    for specifier in child
                        .children(&mut inner)
                        .filter(|c| c.kind() == "export_specifier")
                    {
                        let name = specifier
                            .child_by_field_name("name")
                            .map(|n| self.text(n).to_owned());
                        let alias = specifier
                            .child_by_field_name("alias")
                            .map(|n| self.text(n).to_owned());
                        match &source {
                            Some(spelling) => {
                                let mut fact =
                                    self.fact(specifier, spelling.clone(), ImportForm::ExportFrom);
                                fact.imported_name = name;
                                fact.alias = alias;
                                fact.reexport = true;
                                fact.kind = kind(statement_type || self.has_keyword_in(specifier));
                                self.push(fact);
                            }
                            None => {
                                if let Some(exported) = alias.or(name) {
                                    self.item(specifier, exported, ItemKind::Variable);
                                }
                            }
                        }
                    }
                }
                "namespace_export" => {
                    if let Some(spelling) = &source {
                        let mut fact =
                            self.fact(*child, spelling.clone(), ImportForm::ExportAllFrom);
                        fact.imported_name = Some("*".into());
                        fact.alias = self.last_identifier(*child);
                        fact.reexport = true;
                        fact.kind = kind(statement_type);
                        self.push(fact);
                    }
                }
                _ => {}
            }
        }
        let is_star = children.iter().any(|c| !c.is_named() && c.kind() == "*");
        if let (true, Some(spelling)) = (is_star, &source) {
            let mut fact = self.fact(node, spelling.clone(), ImportForm::ExportAllFrom);
            fact.imported_name = Some("*".into());
            fact.glob = true;
            fact.reexport = true;
            fact.kind = kind(statement_type);
            self.push(fact);
        }
        if children
            .iter()
            .any(|c| !c.is_named() && c.kind() == "default")
        {
            self.item(node, "default".into(), ItemKind::Variable);
        }
    }

    fn declaration_items(&mut self, node: Node) {
        let item_kind = match node.kind() {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                ItemKind::Function
            }
            "class_declaration" | "abstract_class_declaration" => ItemKind::Class,
            "interface_declaration" => ItemKind::Interface,
            "type_alias_declaration" => ItemKind::TypeAlias,
            "enum_declaration" => ItemKind::Enum,
            "internal_module" | "module" => ItemKind::Module,
            "lexical_declaration" | "variable_declaration" => {
                let mut cursor = node.walk();
                let declarators = node
                    .children(&mut cursor)
                    .filter(|c| c.kind() == "variable_declarator")
                    .collect::<Vec<_>>();
                for declarator in declarators {
                    if let Some(name) = declarator.child_by_field_name("name") {
                        if name.kind() == "identifier" {
                            self.item(declarator, self.text(name).to_owned(), ItemKind::Variable);
                        }
                    }
                }
                return;
            }
            _ => return,
        };
        if let Some(name) = node.child_by_field_name("name") {
            self.item(
                node,
                self.text(name).trim_matches(['"', '\'']).to_owned(),
                item_kind,
            );
        }
    }

    fn item(&mut self, node: Node, name: String, kind: ItemKind) {
        self.out.items.push(DeclaredItem {
            name,
            kind,
            visibility: Visibility::Public,
            span: span(node),
            scope: Vec::new(),
        });
    }

    fn call_expression(&mut self, node: Node) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let form = match function.kind() {
            "import" => ImportForm::DynamicImport,
            "identifier" if self.text(function) == "require" => ImportForm::Require,
            _ => return,
        };
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let mut cursor = arguments.walk();
        let Some(argument) = arguments.named_children(&mut cursor).next() else {
            return;
        };
        let (spelling, literal) = match self.literal(argument) {
            Some(literal) => (literal, true),
            None => (
                self.text(argument).chars().take(MAX_SPELLING).collect(),
                false,
            ),
        };
        let mut fact = self.fact(node, spelling, form);
        fact.literal_specifier = literal;
        if form == ImportForm::DynamicImport || !literal {
            fact.staticness = Staticness::Dynamic;
        }
        self.push(fact);
    }
}

fn kind(type_only: bool) -> ImportKind {
    if type_only {
        ImportKind::TypeOnly
    } else {
        ImportKind::Runtime
    }
}
