//! Python import extraction: `import`, `from … import`, relative imports, and literal
//! `importlib.import_module` / `__import__` calls.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportKind, ImportLimits,
        ItemKind, LanguageImportAdapter, LanguageProfile, Staticness, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct PythonImportAdapter;

impl LanguageImportAdapter for PythonImportAdapter {
    fn id(&self) -> &'static str {
        "python-imports"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension()?.to_str()? == "py").then_some(LanguageProfile::Python)
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Import, ImportForm::DynamicImport]
    }

    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        let mut out = ExtractedFile::default();
        let mut parser = tree_sitter::Parser::new();
        if parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .is_err()
        {
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
        let builder = FactBuilder { path, profile };
        let root = tree.root_node();
        collect_items(root, source, &mut out);
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if out.facts.len() >= limits.max_facts_per_file {
                out.truncated = true;
                break;
            }
            match node.kind() {
                "import_statement" => import_statement(node, source, &builder, &mut out),
                "import_from_statement" => from_statement(node, source, &builder, &mut out),
                "call" => dynamic_call(node, source, &builder, &mut out),
                _ => {}
            }
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        out.facts.sort_by_key(|f| (f.span.start, f.id.0.clone()));
        out
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn span(node: Node<'_>) -> ByteSpan {
    ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

/// Inside `if TYPE_CHECKING:` (or `typing.TYPE_CHECKING`) an import is only for type checkers.
fn type_only(node: Node<'_>, source: &str) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.kind() == "if_statement" {
            if let Some(condition) = ancestor.child_by_field_name("condition") {
                let condition = text(condition, source);
                let in_consequence = ancestor
                    .child_by_field_name("consequence")
                    .is_some_and(|c| {
                        c.start_byte() <= node.start_byte() && node.end_byte() <= c.end_byte()
                    });
                if in_consequence
                    && (condition == "TYPE_CHECKING" || condition.ends_with(".TYPE_CHECKING"))
                {
                    return true;
                }
            }
        }
        current = ancestor.parent();
    }
    false
}

fn import_statement(
    node: Node<'_>,
    source: &str,
    builder: &FactBuilder<'_>,
    out: &mut ExtractedFile,
) {
    let mut cursor = node.walk();
    let kind = if type_only(node, source) {
        ImportKind::TypeOnly
    } else {
        ImportKind::Runtime
    };
    for name in node.children_by_field_name("name", &mut cursor) {
        let (module, alias) = match name.kind() {
            "aliased_import" => (
                name.child_by_field_name("name").map(|n| text(n, source)),
                name.child_by_field_name("alias")
                    .map(|n| text(n, source).to_owned()),
            ),
            _ => (Some(text(name, source)), None),
        };
        let Some(module) = module else { continue };
        let mut fact = builder.fact(span(name), "", module.to_owned(), ImportForm::Import);
        fact.alias = alias;
        fact.kind = kind;
        out.facts.push(fact);
    }
}

fn from_statement(
    node: Node<'_>,
    source: &str,
    builder: &FactBuilder<'_>,
    out: &mut ExtractedFile,
) {
    let Some(module) = node.child_by_field_name("module_name") else {
        return;
    };
    let module_text = text(module, source).to_owned();
    let kind = if type_only(node, source) {
        ImportKind::TypeOnly
    } else {
        ImportKind::Runtime
    };
    let mut cursor = node.walk();
    let names: Vec<Node<'_>> = node.children_by_field_name("name", &mut cursor).collect();
    let mut found_any = false;
    for name in names {
        let (imported, alias) = match name.kind() {
            "aliased_import" => (
                name.child_by_field_name("name")
                    .map(|n| text(n, source).to_owned()),
                name.child_by_field_name("alias")
                    .map(|n| text(n, source).to_owned()),
            ),
            _ => (Some(text(name, source).to_owned()), None),
        };
        let Some(imported) = imported else { continue };
        let mut fact = builder.fact(
            span(name),
            &imported,
            module_text.clone(),
            ImportForm::Import,
        );
        fact.imported_name = Some(imported);
        fact.alias = alias;
        fact.kind = kind;
        out.facts.push(fact);
        found_any = true;
    }
    let mut inner = node.walk();
    if node
        .children(&mut inner)
        .any(|c| c.kind() == "wildcard_import")
    {
        let mut fact = builder.fact(span(node), "*", module_text, ImportForm::Import);
        fact.imported_name = Some("*".into());
        fact.glob = true;
        fact.kind = kind;
        out.facts.push(fact);
    } else if !found_any && module_text != "__future__" {
        out.diagnostics.push(format!(
            "`from {module_text} import …` names nothing readable"
        ));
    }
}

fn dynamic_call(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if !matches!(
        text(function, source),
        "importlib.import_module" | "import_module" | "__import__"
    ) {
        return;
    }
    let argument = node
        .child_by_field_name("arguments")
        .and_then(|a| a.named_child(0));
    let literal = argument.filter(|a| a.kind() == "string").and_then(|a| {
        let mut cursor = a.walk();
        let mut contents = String::new();
        for part in a.named_children(&mut cursor) {
            match part.kind() {
                "string_content" => contents.push_str(text(part, source)),
                "interpolation" => return None,
                _ => {}
            }
        }
        Some(contents)
    });
    let mut fact = builder.fact(
        span(node),
        "",
        literal
            .clone()
            .unwrap_or_else(|| argument.map_or(String::new(), |a| text(a, source).to_owned())),
        ImportForm::DynamicImport,
    );
    fact.staticness = Staticness::Dynamic;
    fact.literal_specifier = literal.is_some();
    out.facts.push(fact);
}

fn collect_items(root: Node<'_>, source: &str, out: &mut ExtractedFile) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let node = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition").unwrap_or(child)
        } else {
            child
        };
        let (kind, name) = match node.kind() {
            "function_definition" => (ItemKind::Function, node.child_by_field_name("name")),
            "class_definition" => (ItemKind::Class, node.child_by_field_name("name")),
            "expression_statement" => {
                let assignment = node.named_child(0).filter(|n| n.kind() == "assignment");
                (
                    ItemKind::Variable,
                    assignment
                        .and_then(|a| a.child_by_field_name("left"))
                        .filter(|l| l.kind() == "identifier"),
                )
            }
            "import_statement" | "import_from_statement" => {
                // A name bound by a top-level import is also offered to importers (a re-export).
                let mut inner = node.walk();
                for bound in node.children_by_field_name("name", &mut inner) {
                    let (spelled, alias) = match bound.kind() {
                        "aliased_import" => (
                            bound.child_by_field_name("name"),
                            bound.child_by_field_name("alias"),
                        ),
                        _ => (Some(bound), None),
                    };
                    let local = alias.or(spelled).map(|n| text(n, source)).map(|t| {
                        if node.kind() == "import_statement" {
                            t.split('.').next().unwrap_or(t)
                        } else {
                            t
                        }
                    });
                    if let Some(local) = local {
                        out.items.push(DeclaredItem {
                            visibility: if local.starts_with('_') {
                                Visibility::Private
                            } else {
                                Visibility::Public
                            },
                            name: local.to_owned(),
                            kind: ItemKind::Variable,
                            span: span(bound),
                            scope: Vec::new(),
                        });
                    }
                }
                continue;
            }
            _ => continue,
        };
        let Some(name) = name else { continue };
        let name_text = text(name, source).to_owned();
        out.items.push(DeclaredItem {
            visibility: if name_text.starts_with('_') {
                Visibility::Private
            } else {
                Visibility::Public
            },
            name: name_text,
            kind,
            span: span(name),
            scope: Vec::new(),
        });
    }
}
