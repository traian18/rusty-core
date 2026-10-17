//! PHP import extraction: `use` declarations (plain, grouped, aliased, function and const),
//! literal `require`/`include`, and the namespaces and types a file declares.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportLimits, ItemKind,
        LanguageImportAdapter, LanguageProfile, Staticness, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

/// The item kind that stands for a `namespace` (its name is the backslash-separated namespace).
pub const NAMESPACE_ITEM: ItemKind = ItemKind::Module;

#[derive(Clone, Copy, Debug, Default)]
pub struct PhpImportAdapter;

impl LanguageImportAdapter for PhpImportAdapter {
    fn id(&self) -> &'static str {
        "php-imports"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension()?.to_str()? == "php").then_some(LanguageProfile::Php)
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Use, ImportForm::Require]
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
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
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
        let mut namespace = String::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if out.facts.len() >= limits.max_facts_per_file {
                out.truncated = true;
                break;
            }
            match node.kind() {
                "namespace_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        namespace = text(name, source).trim_start_matches('\\').to_owned();
                        out.items.push(DeclaredItem {
                            name: namespace.clone(),
                            kind: NAMESPACE_ITEM,
                            visibility: Visibility::Public,
                            span: span(name),
                            scope: Vec::new(),
                        });
                    }
                }
                "namespace_use_declaration" => use_declaration(node, source, &builder, &mut out),
                "require_expression"
                | "require_once_expression"
                | "include_expression"
                | "include_once_expression" => require(node, source, &builder, &mut out),
                "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        let kind = match node.kind() {
                            "interface_declaration" | "trait_declaration" => ItemKind::Interface,
                            "enum_declaration" => ItemKind::Enum,
                            _ => ItemKind::Class,
                        };
                        out.items.push(DeclaredItem {
                            name: text(name, source).to_owned(),
                            kind,
                            visibility: Visibility::Public,
                            span: span(name),
                            scope: vec![namespace.clone()],
                        });
                    }
                }
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

fn use_declaration(
    node: Node<'_>,
    source: &str,
    builder: &FactBuilder<'_>,
    out: &mut ExtractedFile,
) {
    let declaration = text(node, source);
    let kind = if declaration.starts_with("use function") {
        Some("function")
    } else if declaration.starts_with("use const") {
        Some("const")
    } else {
        None
    };
    let mut push = |clause: Node<'_>, prefix: &str| {
        let (path, alias) = {
            let mut cursor = clause.walk();
            let path = clause
                .named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "qualified_name" | "name" | "namespace_name"))
                .map(|c| text(c, source).to_owned());
            (
                path,
                clause
                    .child_by_field_name("alias")
                    .map(|a| text(a, source).to_owned()),
            )
        };
        let Some(path) = path else { return };
        let full = if prefix.is_empty() {
            path
        } else {
            format!("{}\\{}", prefix.trim_end_matches('\\'), path)
        };
        let full = full.trim_start_matches('\\').to_owned();
        let last = full.rsplit('\\').next().unwrap_or(&full).to_owned();
        let mut fact = builder.fact(span(clause), "", full, ImportForm::Use);
        fact.imported_name = Some(last);
        fact.alias = alias;
        if let Some(kind) = kind {
            fact.scope = vec![kind.into()];
        }
        out.facts.push(fact);
    };
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let group = children
        .iter()
        .find(|c| c.kind() == "namespace_use_group")
        .copied();
    let prefix = children
        .iter()
        .find(|c| c.kind() == "namespace_name")
        .map(|c| text(*c, source).to_owned())
        .unwrap_or_default();
    match group {
        Some(group) => {
            let mut inner = group.walk();
            for clause in group
                .named_children(&mut inner)
                .filter(|c| c.kind() == "namespace_use_clause")
            {
                push(clause, &prefix);
            }
        }
        None => {
            for clause in children
                .iter()
                .filter(|c| c.kind() == "namespace_use_clause")
            {
                push(*clause, "");
            }
        }
    }
}

/// The literal tail of a path expression: `'x.php'`, `__DIR__ . '/x.php'`, `dirname(__DIR__) . '/x.php'`.
fn literal_path(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "string" | "encapsed_string" => {
            let mut cursor = node.walk();
            let mut out = String::new();
            for part in node.named_children(&mut cursor) {
                match part.kind() {
                    "string_content" | "string_value" => out.push_str(text(part, source)),
                    _ => return None,
                }
            }
            Some(out)
        }
        "parenthesized_expression" => literal_path(node.named_child(0)?, source),
        "binary_expression" => {
            // Only the shapes whose left side is the including file's own directory.
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let left_text = text(left, source).replace(' ', "");
            let anchored = left_text == "__DIR__" || left_text.starts_with("dirname(__DIR__");
            anchored
                .then(|| literal_path(right, source))
                .flatten()
                .map(|p| {
                    if left_text.starts_with("dirname(__DIR__") {
                        format!("../{}", p.trim_start_matches('/'))
                    } else {
                        p
                    }
                })
        }
        _ => None,
    }
}

fn require(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let Some(argument) = node.named_child(0) else {
        return;
    };
    let literal = literal_path(argument, source);
    let mut fact = builder.fact(
        span(node),
        "",
        literal
            .clone()
            .unwrap_or_else(|| text(argument, source).to_owned()),
        ImportForm::Require,
    );
    if literal.is_none() {
        fact.staticness = Staticness::Dynamic;
        fact.literal_specifier = false;
    }
    out.facts.push(fact);
}
