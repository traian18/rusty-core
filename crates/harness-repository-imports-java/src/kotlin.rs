//! Kotlin import extraction: `import a.b.C`, `import a.b.*` and `import a.b.C as D`, plus the
//! file's package and its top-level declarations. Kotlin imports name classes, objects, type
//! aliases, functions and properties alike, so every top-level declaration is offered.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, FactBuilder, ImportForm, ImportLimits, ItemKind,
        LanguageProfile, Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

use crate::extract::PACKAGE_ITEM;

pub(crate) fn extract(
    path: &RootRelativePath,
    profile: LanguageProfile,
    source: &str,
    limits: &ImportLimits,
) -> ExtractedFile {
    let mut out = ExtractedFile::default();
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
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
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "package_header" => {
                if let Some(name) = qualified(child) {
                    out.items.push(item(
                        name,
                        text(name, source),
                        PACKAGE_ITEM,
                        Visibility::Public,
                    ));
                }
            }
            "import" => {
                if out.facts.len() >= limits.max_facts_per_file {
                    out.truncated = true;
                    continue;
                }
                import(child, source, &builder, &mut out);
            }
            _ => declaration(child, source, &mut out),
        }
    }
    out
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn qualified(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|n| matches!(n.kind(), "qualified_identifier" | "identifier"));
    found
}

fn item(node: Node<'_>, name: &str, kind: ItemKind, visibility: Visibility) -> DeclaredItem {
    DeclaredItem {
        name: name.to_owned(),
        kind,
        visibility,
        span: ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        },
        scope: Vec::new(),
    }
}

/// `private` declarations are file-local and `internal` ones module-local; the rest are public.
fn visibility(node: Node<'_>, source: &str) -> Visibility {
    let mut cursor = node.walk();
    let modifiers = node
        .children(&mut cursor)
        .find(|n| n.kind() == "modifiers")
        .map(|m| text(m, source))
        .unwrap_or("");
    let has = |word: &str| modifiers.split_whitespace().any(|w| w == word);
    if has("private") {
        Visibility::Private
    } else if has("internal") {
        Visibility::Crate
    } else {
        Visibility::Public
    }
}

fn declaration(node: Node<'_>, source: &str, out: &mut ExtractedFile) {
    let kind = match node.kind() {
        "class_declaration" => {
            let mut cursor = node.walk();
            let keywords: Vec<&str> = node.children(&mut cursor).map(|c| c.kind()).collect();
            if keywords.contains(&"interface") {
                ItemKind::Interface
            } else if node.child(0).is_some_and(|m| {
                m.kind() == "modifiers" && text(m, source).split_whitespace().any(|w| w == "enum")
            }) {
                ItemKind::Enum
            } else {
                ItemKind::Class
            }
        }
        "object_declaration" => ItemKind::Class,
        "function_declaration" => ItemKind::Function,
        "type_alias" => ItemKind::TypeAlias,
        "property_declaration" => ItemKind::Variable,
        _ => return,
    };
    let name = match node.kind() {
        "type_alias" => node.child_by_field_name("type"),
        "property_declaration" => {
            let mut cursor = node.walk();
            let declaration = node
                .named_children(&mut cursor)
                .find(|n| n.kind() == "variable_declaration");
            declaration.and_then(|d| d.named_child(0))
        }
        _ => node.child_by_field_name("name"),
    };
    if let Some(name) = name.filter(|n| n.kind() == "identifier") {
        out.items.push(item(
            name,
            text(name, source),
            kind,
            visibility(node, source),
        ));
    }
}

fn import(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let Some(name) = qualified(node) else {
        return;
    };
    let full = text(name, source).to_owned();
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    let wildcard = children.iter().any(|c| c.kind() == "*");
    // `import a.b.C as D`: the identifier after `as`.
    let alias = children
        .iter()
        .skip_while(|c| c.kind() != "as")
        .find(|c| c.kind() == "identifier")
        .map(|a| text(*a, source).to_owned());
    let span = ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    };
    let mut fact = builder.fact(span, "", full.clone(), ImportForm::Import);
    fact.glob = wildcard;
    fact.imported_name = Some(if wildcard {
        "*".into()
    } else {
        full.rsplit('.').next().unwrap_or(&full).to_owned()
    });
    fact.alias = alias;
    out.facts.push(fact);
}
