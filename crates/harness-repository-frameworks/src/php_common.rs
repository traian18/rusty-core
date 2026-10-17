//! What the PHP framework adapters share: classes with attributes and constructor parameters,
//! string literals, call arguments, and a package-wide index of classes by name.
use std::collections::HashMap;

use harness_workspace::{
    framework_relations::FrameworkInput,
    repository_intelligence::{Package, RootRelativePath},
};
use tree_sitter::Node;

use crate::syntax::{text, walk};

pub fn php_files(package: &Package, input: &FrameworkInput<'_>) -> Vec<RootRelativePath> {
    input
        .files_of(package)
        .into_iter()
        .filter(|f| f.path.as_path().extension().and_then(|e| e.to_str()) == Some("php"))
        .map(|f| f.path.clone())
        .collect()
}

/// The text of a plain string literal.
pub fn string_value(node: Node<'_>, source: &str) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
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

/// `(name, value)` of each argument of a call or attribute; `name` is set for named arguments.
pub fn arguments<'t>(node: Node<'t>, field: &str, source: &str) -> Vec<(Option<String>, Node<'t>)> {
    let Some(list) = node.child_by_field_name(field) else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|a| a.kind() == "argument")
        .filter_map(|a| {
            let name = a
                .child_by_field_name("name")
                .map(|n| text(n, source).to_owned());
            let mut inner = a.walk();
            let value = a
                .named_children(&mut inner)
                .filter(|c| c.kind() != "name" || name.is_none())
                .last()?;
            Some((name, value))
        })
        .collect()
}

pub struct Attribute<'t> {
    /// The last segment of the attribute name: `Route`, `Entity` for `ORM\Entity`.
    pub name: String,
    pub arguments: Vec<(Option<String>, Node<'t>)>,
    pub node: Node<'t>,
}

impl<'t> Attribute<'t> {
    /// A named argument, or else the first positional one.
    pub fn argument(&self, name: &str) -> Option<Node<'t>> {
        self.arguments
            .iter()
            .find(|(n, _)| n.as_deref() == Some(name))
            .map(|(_, v)| *v)
    }

    pub fn first(&self) -> Option<Node<'t>> {
        self.arguments
            .iter()
            .find(|(n, _)| n.is_none())
            .map(|(_, v)| *v)
    }
}

pub struct Parameter<'t> {
    pub type_name: String,
    pub node: Node<'t>,
}

pub struct Method<'t> {
    pub name: String,
    pub attributes: Vec<Attribute<'t>>,
    pub parameters: Vec<Parameter<'t>>,
}

pub struct Class<'t> {
    pub name: String,
    pub base: Option<String>,
    pub attributes: Vec<Attribute<'t>>,
    pub methods: Vec<Method<'t>>,
    /// `protected $name = 'literal';` properties.
    pub properties: HashMap<String, String>,
    pub node: Node<'t>,
}

impl Class<'_> {
    pub fn attribute(&self, name: &str) -> Option<&Attribute<'_>> {
        self.attributes.iter().find(|a| a.name == name)
    }

    pub fn constructor(&self) -> Option<&Method<'_>> {
        self.methods.iter().find(|m| m.name == "__construct")
    }
}

fn attributes<'t>(declaration: Node<'t>, source: &str) -> Vec<Attribute<'t>> {
    let mut found = Vec::new();
    let Some(list) = declaration.child_by_field_name("attributes") else {
        return found;
    };
    walk(list, &mut |node| {
        if node.kind() != "attribute" {
            return;
        }
        let Some(name) = node
            .named_child(0)
            .filter(|n| matches!(n.kind(), "name" | "qualified_name"))
        else {
            return;
        };
        found.push(Attribute {
            name: text(name, source)
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .to_owned(),
            arguments: arguments(node, "parameters", source),
            node,
        });
    });
    found
}

fn simple_type(node: Node<'_>, source: &str) -> Option<String> {
    let ty = node.child_by_field_name("type")?;
    let name = match ty.kind() {
        "named_type" => ty.named_child(0)?,
        "optional_type" => ty.named_child(0)?.named_child(0)?,
        _ => return None,
    };
    Some(
        text(name, source)
            .rsplit('\\')
            .next()
            .unwrap_or("")
            .to_owned(),
    )
}

pub fn classes<'t>(root: Node<'t>, source: &str) -> Vec<Class<'t>> {
    let mut found = Vec::new();
    walk(root, &mut |node| {
        if node.kind() != "class_declaration" {
            return;
        }
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut cursor = node.walk();
        let base = node
            .children(&mut cursor)
            .find(|c| c.kind() == "base_clause")
            .and_then(|c| c.named_child(0))
            .map(|b| text(b, source).rsplit('\\').next().unwrap_or("").to_owned());
        let (mut methods, mut properties) = (Vec::new(), HashMap::new());
        if let Some(body) = node.child_by_field_name("body") {
            let mut inner = body.walk();
            for member in body.named_children(&mut inner) {
                match member.kind() {
                    "method_declaration" => {
                        let Some(method_name) = member.child_by_field_name("name") else {
                            continue;
                        };
                        let mut parameters = Vec::new();
                        if let Some(list) = member.child_by_field_name("parameters") {
                            let mut params = list.walk();
                            for parameter in list.named_children(&mut params).filter(|p| {
                                matches!(
                                    p.kind(),
                                    "simple_parameter" | "property_promotion_parameter"
                                )
                            }) {
                                if let Some(type_name) = simple_type(parameter, source) {
                                    parameters.push(Parameter {
                                        type_name,
                                        node: parameter,
                                    });
                                }
                            }
                        }
                        methods.push(Method {
                            name: text(method_name, source).to_owned(),
                            attributes: attributes(member, source),
                            parameters,
                        });
                    }
                    "property_declaration" => {
                        let mut elements = member.walk();
                        for element in member
                            .named_children(&mut elements)
                            .filter(|e| e.kind() == "property_element")
                        {
                            let property = element
                                .child_by_field_name("name")
                                .map(|n| text(n, source).trim_start_matches('$').to_owned());
                            let value = element
                                .child_by_field_name("default_value")
                                .and_then(|v| string_value(v, source));
                            if let (Some(property), Some(value)) = (property, value) {
                                properties.insert(property, value);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        found.push(Class {
            name: text(name, source).to_owned(),
            base,
            attributes: attributes(node, source),
            methods,
            properties,
            node,
        });
    });
    found
}

/// Class name -> files that declare it, over a package's PHP files.
pub fn class_index(
    parsed: &[(RootRelativePath, String, tree_sitter::Tree)],
) -> HashMap<String, Vec<RootRelativePath>> {
    let mut index: HashMap<String, Vec<RootRelativePath>> = HashMap::new();
    for (file, source, tree) in parsed {
        for class in classes(tree.root_node(), source) {
            index.entry(class.name).or_default().push(file.clone());
        }
    }
    index
}

/// `PhotoController::class` or a plain string: the simple class name it names.
pub fn class_reference(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "class_constant_access_expression" => {
            let mut cursor = node.walk();
            let parts: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            (parts.last().map(|p| text(*p, source)) == Some("class"))
                .then(|| {
                    parts.first().map(|c| {
                        text(*c, source)
                            .rsplit('\\')
                            .next()
                            .unwrap_or("")
                            .to_owned()
                    })
                })
                .flatten()
        }
        "string" => {
            string_value(node, source).map(|s| s.rsplit('\\').next().unwrap_or("").to_owned())
        }
        _ => None,
    }
}

/// `UserAccount` becomes `user_account`.
pub fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// A naive English plural, as Laravel's table naming does: `user` -> `users`, `category` -> `categories`.
pub fn plural(word: &str) -> String {
    if let Some(stem) = word
        .strip_suffix('y')
        .filter(|s| !s.ends_with(['a', 'e', 'i', 'o', 'u']))
    {
        format!("{stem}ies")
    } else if word.ends_with(['s', 'x', 'z']) || word.ends_with("ch") || word.ends_with("sh") {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}
