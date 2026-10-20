//! YAML as a value tree with line numbers. Anchors, tags and aliases are read as plain values.
use tree_sitter::{Node, Parser};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node_ {
    pub value: Value,
    /// 1-based first and last line.
    pub line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Map(Vec<Entry>),
    Seq(Vec<Node_>),
    Scalar(String),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub key_line: u32,
    pub value: Node_,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Yaml {
    pub documents: Vec<Node_>,
    pub partial: bool,
}

impl Node_ {
    pub fn get(&self, key: &str) -> Option<&Node_> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|e| e.key == key).map(|e| &e.value),
            _ => None,
        }
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|e| e.key == key),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        match &self.value {
            Value::Map(entries) => entries,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Node_] {
        match &self.value {
            Value::Seq(items) => items,
            _ => &[],
        }
    }

    /// Calls `f` on every mapping entry at any depth, in document order.
    pub fn for_each_entry<'a>(&'a self, f: &mut impl FnMut(&'a Entry)) {
        match &self.value {
            Value::Map(entries) => {
                for entry in entries {
                    f(entry);
                    entry.value.for_each_entry(f);
                }
            }
            Value::Seq(items) => items.iter().for_each(|i| i.for_each_entry(f)),
            _ => {}
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::Scalar(s) => Some(s),
            _ => None,
        }
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn lines(node: Node<'_>) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

fn unquote(raw: &str) -> String {
    let t = raw.trim();
    for q in ['"', '\''] {
        if let Some(inner) = t.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner.to_owned();
        }
    }
    t.to_owned()
}

fn block_scalar(raw: &str) -> String {
    raw.split_once('\n')
        .map(|(_, rest)| {
            rest.lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

fn convert(node: Node<'_>, source: &str) -> Node_ {
    let (line, end_line) = lines(node);
    let value = match node.kind() {
        "block_node" | "flow_node" | "document" => {
            let mut cursor = node.walk();
            let inner = node
                .named_children(&mut cursor)
                .find(|c| !matches!(c.kind(), "anchor" | "tag" | "comment" | "directive"));
            return match inner {
                Some(inner) => {
                    let mut converted = convert(inner, source);
                    converted.line = line;
                    converted.end_line = end_line;
                    converted
                }
                None => Node_ {
                    value: Value::Null,
                    line,
                    end_line,
                },
            };
        }
        "block_mapping" | "flow_mapping" => {
            let mut entries = Vec::new();
            let mut cursor = node.walk();
            for pair in node.named_children(&mut cursor) {
                if !matches!(pair.kind(), "block_mapping_pair" | "flow_pair") {
                    continue;
                }
                let Some(key) = pair.child_by_field_name("key") else {
                    continue;
                };
                let value = pair
                    .child_by_field_name("value")
                    .map(|v| convert(v, source))
                    .unwrap_or(Node_ {
                        value: Value::Null,
                        line: lines(pair).0,
                        end_line: lines(pair).1,
                    });
                entries.push(Entry {
                    key: unquote(text(key, source)),
                    key_line: lines(key).0,
                    value,
                });
            }
            Value::Map(entries)
        }
        "block_sequence" | "flow_sequence" => {
            let mut items = Vec::new();
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                match item.kind() {
                    "block_sequence_item" => {
                        let mut c = item.walk();
                        let inner = item.named_children(&mut c).find(|n| n.kind() != "comment");
                        items.push(match inner {
                            Some(n) => convert(n, source),
                            None => Node_ {
                                value: Value::Null,
                                line: lines(item).0,
                                end_line: lines(item).1,
                            },
                        });
                    }
                    "comment" => {}
                    _ => items.push(convert(item, source)),
                }
            }
            Value::Seq(items)
        }
        "block_scalar" => Value::Scalar(block_scalar(text(node, source))),
        "plain_scalar" | "double_quote_scalar" | "single_quote_scalar" | "alias" => {
            Value::Scalar(unquote(text(node, source)))
        }
        _ => Value::Scalar(text(node, source).trim().to_owned()),
    };
    Node_ {
        value,
        line,
        end_line,
    }
}

pub fn parse(source: &str) -> Yaml {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .is_err()
    {
        return Yaml::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Yaml::default();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let documents = root
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "document")
        .map(|d| convert(d, source))
        .collect();
    Yaml {
        documents,
        partial: root.has_error(),
    }
}
