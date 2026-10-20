//! HCL (Terraform and Terragrunt) as blocks and attributes with line numbers.
use tree_sitter::{Node, Parser};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    pub attrs: Vec<Attr>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    /// The expression exactly as written.
    pub value: String,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: String,
    pub labels: Vec<String>,
    pub body: Body,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hcl {
    pub body: Body,
    /// The parse had errors; the model covers what parsed.
    pub partial: bool,
}

impl Body {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    pub fn blocks_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Block> {
        self.blocks.iter().filter(move |b| b.kind == kind)
    }
}

impl Attr {
    /// The string when the value is a plain literal without interpolation.
    pub fn literal(&self) -> Option<&str> {
        let v = self.value.trim();
        let inner = v.strip_prefix('"')?.strip_suffix('"')?;
        (!inner.contains("${") && !inner.contains('"')).then_some(inner)
    }
}

/// Whether `object` (an HCL object expression as written) defines `key`.
pub fn object_has_key(object: &str, key: &str) -> bool {
    let mut rest = object;
    while let Some(at) = rest.find(key) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + key.len()..].trim_start();
        let word_start = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
        if word_start && (after.starts_with('=') || after.starts_with(':')) {
            return true;
        }
        rest = &rest[at + key.len()..];
    }
    false
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn label(node: Node<'_>, source: &str) -> String {
    text(node, source).trim_matches('"').to_owned()
}

fn body(node: Node<'_>, source: &str) -> Body {
    let mut out = Body::default();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute" => {
                let mut c = child.walk();
                let mut parts = child.named_children(&mut c);
                let name = parts.next().map(|n| text(n, source).to_owned());
                let value = parts.next().map(|n| text(n, source).to_owned());
                if let (Some(name), Some(value)) = (name, value) {
                    out.attrs.push(Attr {
                        name,
                        value,
                        line: child.start_position().row as u32 + 1,
                        end_line: child.end_position().row as u32 + 1,
                    });
                }
            }
            "block" => {
                let mut kind = String::new();
                let mut labels = Vec::new();
                let mut inner = Body::default();
                let mut c = child.walk();
                for part in child.named_children(&mut c) {
                    match part.kind() {
                        "identifier" if kind.is_empty() => kind = text(part, source).to_owned(),
                        "identifier" | "string_lit" => labels.push(label(part, source)),
                        "body" => inner = body(part, source),
                        _ => {}
                    }
                }
                out.blocks.push(Block {
                    kind,
                    labels,
                    body: inner,
                    line: child.start_position().row as u32 + 1,
                    end_line: child.end_position().row as u32 + 1,
                });
            }
            _ => {}
        }
    }
    out
}

pub fn parse(source: &str) -> Hcl {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_hcl::LANGUAGE.into())
        .is_err()
    {
        return Hcl::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Hcl::default();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let top = root
        .named_children(&mut cursor)
        .find(|n| n.kind() == "body")
        .map(|n| body(n, source))
        .unwrap_or_default();
    Hcl {
        body: top,
        partial: root.has_error(),
    }
}
