//! Rust import extraction: `use` leaves, `mod`/`extern crate` declarations, and module-level items.
use harness_workspace::{
    import_graph::{
        ByteSpan, DeclaredItem, ExtractedFile, ImportFact, ImportFactId, ImportForm, ImportKind,
        ImportLimits, ItemKind, LanguageImportAdapter, LanguageProfile, PathAttribute, Staticness,
        Visibility,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::{Node, Parser};

pub const RUST_ADAPTER_VERSION: u32 = 1;
const MAX_DEPTH: usize = 200;

#[derive(Clone, Copy, Debug, Default)]
pub struct RustImportAdapter;

impl LanguageImportAdapter for RustImportAdapter {
    fn id(&self) -> &'static str {
        "rust-imports"
    }

    fn version(&self) -> u32 {
        RUST_ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        (path.as_path().extension().and_then(|e| e.to_str()) == Some("rs"))
            .then_some(LanguageProfile::Rust)
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Use, ImportForm::Mod, ImportForm::ExternCrate]
    }

    fn extract(
        &self,
        path: &RootRelativePath,
        _profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        let mut out = ExtractedFile::default();
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .is_err()
        {
            out.diagnostics.push("Rust grammar is unavailable".into());
            return out;
        }
        let Some(tree) = parser.parse(source, None) else {
            out.diagnostics.push("Rust parse produced no tree".into());
            return out;
        };
        if tree.root_node().has_error() {
            out.diagnostics
                .push("Rust source has syntax errors; extraction may be incomplete".into());
        }
        let mut walker = Walker {
            path,
            source,
            limits,
            out,
            scope: Vec::new(),
        };
        walker.walk(tree.root_node(), true, 0);
        walker.out
    }
}

struct Walker<'a> {
    path: &'a RootRelativePath,
    source: &'a str,
    limits: &'a ImportLimits,
    out: ExtractedFile,
    scope: Vec<String>,
}

struct Leaf {
    segments: Vec<String>,
    span: ByteSpan,
    alias: Option<String>,
    glob: bool,
}

impl<'a> Walker<'a> {
    fn text(&self, node: Node) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn span(node: Node) -> ByteSpan {
        ByteSpan {
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }

    fn walk(&mut self, node: Node, module_level: bool, depth: usize) {
        if depth > MAX_DEPTH {
            self.out
                .diagnostics
                .push("nesting too deep; subtree skipped".into());
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "use_declaration" => self.use_declaration(child),
                "mod_item" if module_level => self.mod_item(child, depth),
                "extern_crate_declaration" if module_level => self.extern_crate(child),
                kind if module_level && item_kind(kind).is_some() => {
                    self.item(child);
                    self.walk(child, false, depth + 1);
                }
                _ => self.walk(child, false, depth + 1),
            }
        }
    }

    fn full(&self) -> bool {
        self.out.facts.len() >= self.limits.max_facts_per_file
    }

    fn push_fact(&mut self, fact: ImportFact) {
        if self.full() {
            self.out.truncated = true;
        } else {
            self.out.facts.push(fact);
        }
    }

    fn fact(&self, span: ByteSpan, spelling: String, form: ImportForm) -> ImportFact {
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
            profile: LanguageProfile::Rust,
            staticness: Staticness::Static,
            kind: ImportKind::Runtime,
            scope: self.scope.clone(),
            imported_name: None,
            alias: None,
            glob: false,
            reexport: false,
            literal_specifier: true,
            path_attribute: PathAttribute::None,
        }
    }

    fn visibility(&self, node: Node) -> Visibility {
        let mut cursor = node.walk();
        let modifier = node
            .children(&mut cursor)
            .find(|child| child.kind() == "visibility_modifier");
        match modifier.map(|m| self.text(m)) {
            None => Visibility::Private,
            Some("pub") => Visibility::Public,
            Some(text) if text.replace(' ', "") == "pub(crate)" => Visibility::Crate,
            Some(_) => Visibility::Restricted,
        }
    }

    fn use_declaration(&mut self, node: Node) {
        let Some(argument) = node.child_by_field_name("argument") else {
            return;
        };
        let reexport = self.visibility(node) != Visibility::Private;
        let mut leaves = Vec::new();
        self.flatten(argument, &[], None, &mut leaves);
        for leaf in leaves {
            let mut fact = self.fact(leaf.span, leaf.segments.join("::"), ImportForm::Use);
            fact.imported_name = leaf.segments.last().cloned();
            fact.alias = leaf.alias;
            fact.glob = leaf.glob;
            fact.reexport = reexport;
            self.push_fact(fact);
        }
    }

    fn flatten(&self, node: Node, prefix: &[String], alias: Option<String>, out: &mut Vec<Leaf>) {
        let span = Self::span(node);
        match node.kind() {
            "use_as_clause" => {
                let alias = node
                    .child_by_field_name("alias")
                    .map(|a| self.text(a).to_owned());
                if let Some(path) = node.child_by_field_name("path") {
                    self.flatten(path, prefix, alias, out);
                }
            }
            "use_wildcard" => {
                let mut segments = prefix.to_vec();
                let mut cursor = node.walk();
                if let Some(path) = node.named_children(&mut cursor).next() {
                    segments.extend(self.path_segments(path));
                }
                out.push(Leaf {
                    segments,
                    span,
                    alias: None,
                    glob: true,
                });
            }
            "scoped_use_list" => {
                let mut segments = prefix.to_vec();
                if let Some(path) = node.child_by_field_name("path") {
                    segments.extend(self.path_segments(path));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    self.flatten(list, &segments, None, out);
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.flatten(child, prefix, None, out);
                }
            }
            "self" if !prefix.is_empty() => out.push(Leaf {
                segments: prefix.to_vec(),
                span,
                alias,
                glob: false,
            }),
            _ => {
                let mut segments = prefix.to_vec();
                segments.extend(self.path_segments(node));
                out.push(Leaf {
                    segments,
                    span,
                    alias,
                    glob: false,
                });
            }
        }
    }

    fn path_segments(&self, node: Node) -> Vec<String> {
        match node.kind() {
            "scoped_identifier" => {
                let mut segments = node
                    .child_by_field_name("path")
                    .map(|p| self.path_segments(p))
                    .unwrap_or_default();
                if let Some(name) = node.child_by_field_name("name") {
                    segments.push(self.text(name).to_owned());
                }
                segments
            }
            _ => vec![self.text(node).to_owned()],
        }
    }

    fn mod_item(&mut self, node: Node, depth: usize) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name).to_owned();
        self.item_named(node, &name, ItemKind::Module);
        match node.child_by_field_name("body") {
            Some(body) => {
                self.scope.push(name);
                self.walk(body, true, depth + 1);
                self.scope.pop();
            }
            None => {
                let mut fact = self.fact(Self::span(node), name.clone(), ImportForm::Mod);
                fact.imported_name = Some(name);
                fact.reexport = false;
                fact.path_attribute = self.path_attribute(node);
                self.push_fact(fact);
            }
        }
    }

    /// Reads `#[path = "..."]` from the attributes directly above a `mod`.
    fn path_attribute(&self, node: Node) -> PathAttribute {
        let mut result = PathAttribute::None;
        let mut previous = node.prev_sibling();
        while let Some(sibling) = previous {
            match sibling.kind() {
                "attribute_item" => {
                    let text = self.text(sibling).replace(char::is_whitespace, "");
                    let inner = text.trim_start_matches("#[").trim_end_matches(']');
                    if let Some(value) = inner.strip_prefix("path=") {
                        let literal = value
                            .strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .filter(|v| !v.contains('"') && !v.contains('\\'));
                        result = match literal {
                            Some(literal) => PathAttribute::Literal(literal.to_owned()),
                            None => PathAttribute::Unsupported,
                        };
                    } else if inner.starts_with("cfg_attr(") && inner.contains("path=") {
                        result = PathAttribute::Unsupported;
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            previous = sibling.prev_sibling();
        }
        result
    }

    fn extern_crate(&mut self, node: Node) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name).to_owned();
        let mut fact = self.fact(Self::span(node), name.clone(), ImportForm::ExternCrate);
        fact.imported_name = Some(name);
        self.push_fact(fact);
    }

    fn item(&mut self, node: Node) {
        let Some(kind) = item_kind(node.kind()) else {
            return;
        };
        if let Some(name) = node.child_by_field_name("name") {
            let name = self.text(name).to_owned();
            self.item_named(node, &name, kind);
        }
    }

    fn item_named(&mut self, node: Node, name: &str, kind: ItemKind) {
        self.out.items.push(DeclaredItem {
            name: name.to_owned(),
            kind,
            visibility: self.visibility(node),
            span: Self::span(node),
            scope: self.scope.clone(),
        });
    }
}

fn item_kind(kind: &str) -> Option<ItemKind> {
    Some(match kind {
        "function_item" => ItemKind::Function,
        "struct_item" | "union_item" => ItemKind::Struct,
        "enum_item" => ItemKind::Enum,
        "trait_item" => ItemKind::Trait,
        "type_item" => ItemKind::TypeAlias,
        "const_item" => ItemKind::Const,
        "static_item" => ItemKind::Static,
        "macro_definition" => ItemKind::Macro,
        _ => return None,
    })
}
