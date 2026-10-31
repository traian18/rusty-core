//! Ruby: literal `require_relative`/`require`, classes and modules with `initialize`, `new`,
//! `self` calls, implicit-self calls on bare identifiers, and blocks/lambdas as closures. Reopened
//! classes, mixins and metaprogramming keep results uncertain.

use tree_sitter::{Language, Node};

use super::util::*;
use crate::adapters::collector::binding_names;
use crate::adapters::collector::{Action, CallSpec, CallableSpec, Collector, Syntax, TypeSpec};
use crate::adapters::facts::*;
use crate::adapters::resolve::{
    dirname, join_normalized, project_span, Peer, ResolutionContext, Scheme,
};
use crate::model::{CallableKind, LanguageId};

pub(crate) struct Ruby;

/// Calls that declare structure or are DSL macros rather than calls into workspace code.
const DECLARATIVE: [&str; 13] = [
    "require",
    "require_relative",
    "include",
    "extend",
    "prepend",
    "lambda",
    "proc",
    "attr_accessor",
    "attr_reader",
    "attr_writer",
    "private",
    "public",
    "protected",
];

impl Syntax for Ruby {
    fn language(&self) -> LanguageId {
        LanguageId::Ruby
    }

    fn grammar(&self, _path: &str) -> Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn limitations(&self) -> Vec<String> {
        vec![
            "resolves literal `require_relative`/`require` of workspace files, methods and classes, `Foo.new` to `initialize`, `Foo.bar`/`self.bar`, implicit-self calls, and receivers typed by `Foo.new`".into(),
            "reopened classes, mixins (`include`/`extend`/`prepend`), `method_missing`, `send`, `define_method`, and `autoload` keep results possible or unresolved".into(),
            "a class defined in several files yields possible targets; instance and class methods of one class are not distinguished".into(),
            "`super` calls and non-literal requires are not resolved".into(),
        ]
    }

    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action {
        match node.kind() {
            "class" | "module" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return Action::Descend;
                };
                let Some(segments) = path_segments(name, c.src) else {
                    return Action::Descend;
                };
                let simple = segments.last().cloned().unwrap_or_default();
                let mut bases = Vec::new();
                if let Some(superclass) = node.child_by_field_name("superclass") {
                    let mut cursor = superclass.walk();
                    for base in superclass.named_children(&mut cursor) {
                        if let Some(path) = path_segments(base, c.src) {
                            bases.extend(path.last().cloned());
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    mixins(body, c.src, &mut bases);
                }
                let leaf = match name.kind() {
                    "scope_resolution" => name.child_by_field_name("name").unwrap_or(name),
                    _ => name,
                };
                Action::Type(TypeSpec {
                    name: simple,
                    name_range: (leaf.start_byte(), leaf.end_byte()),
                    kind: if node.kind() == "module" {
                        TypeKind::Module
                    } else {
                        TypeKind::Class
                    },
                    bases,
                })
            }
            "method" | "singleton_method" => {
                let name = node.child_by_field_name("name");
                let text = name.map(|n| c.text(n).to_owned());
                let in_type = c.current_type().is_some() && !c.in_callable();
                let container = if node.kind() == "singleton_method" {
                    node.child_by_field_name("object")
                        .filter(|object| object.kind() == "constant")
                        .map(|object| c.text(object).to_owned())
                } else {
                    None
                };
                let kind = match (
                    in_type || node.kind() == "singleton_method",
                    text.as_deref(),
                ) {
                    (true, Some("initialize")) => CallableKind::Constructor,
                    (true, _) => CallableKind::Method,
                    _ => CallableKind::Function,
                };
                Action::Callable(CallableSpec {
                    name: text,
                    name_range: name.map(|n| (n.start_byte(), n.end_byte())),
                    kind,
                    container,
                    namespace: None,
                    params: method_params(node.child_by_field_name("parameters"), c),
                })
            }
            "block" | "do_block" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: method_params(node.child_by_field_name("parameters"), c),
            }),
            "lambda" => Action::Callable(CallableSpec {
                name: None,
                name_range: None,
                kind: CallableKind::Closure,
                container: None,
                namespace: None,
                params: method_params(node.child_by_field_name("parameters"), c),
            }),
            "assignment" | "operator_assignment" => {
                self.assignment(node, c);
                Action::Descend
            }
            "for" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let mut names = Vec::new();
                    binding_names(pattern, c.src, &mut names);
                    for name in names {
                        c.declare(&name, false);
                    }
                }
                Action::Descend
            }
            "call" => {
                self.call(node, c);
                Action::Descend
            }
            "identifier" => {
                self.bare_identifier(node, c);
                Action::Skip
            }
            _ => Action::Descend,
        }
    }
}

fn mixins(body: Node<'_>, src: &str, out: &mut Vec<String>) {
    let mut cursor = body.walk();
    for statement in body.named_children(&mut cursor) {
        if statement.kind() != "call" || statement.child_by_field_name("receiver").is_some() {
            continue;
        }
        let Some(method) = statement.child_by_field_name("method") else {
            continue;
        };
        if !matches!(&src[method.byte_range()], "include" | "extend" | "prepend") {
            continue;
        }
        if let Some(arguments) = statement.child_by_field_name("arguments") {
            let mut inner = arguments.walk();
            for argument in arguments.named_children(&mut inner) {
                if let Some(path) = path_segments(argument, src) {
                    out.extend(path.last().cloned());
                }
            }
        }
    }
}

fn method_params(list: Option<Node<'_>>, c: &Collector<'_>) -> Vec<(String, Option<String>)> {
    let Some(list) = list else { return Vec::new() };
    let mut names = Vec::new();
    collect_params(list, c.src, &mut names);
    names.into_iter().map(|n| (n, None)).collect()
}

fn collect_params(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => out.push(src[node.byte_range()].to_owned()),
        "optional_parameter"
        | "keyword_parameter"
        | "splat_parameter"
        | "hash_splat_parameter"
        | "block_parameter" => {
            if let Some(name) = node.child_by_field_name("name") {
                out.push(src[name.byte_range()].to_owned());
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_params(child, src, out);
            }
        }
    }
}

impl Ruby {
    fn assignment(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let right = node.child_by_field_name("right");
        if left.kind() != "identifier" {
            if matches!(
                left.kind(),
                "left_assignment_list" | "destructured_left_assignment"
            ) {
                let mut names = Vec::new();
                binding_names(left, c.src, &mut names);
                for name in names {
                    c.declare(&name, false);
                }
            }
            return;
        }
        let name = c.text(left).to_owned();
        let constructed = right
            .filter(|r| r.kind() == "call")
            .filter(|call| {
                call.child_by_field_name("method")
                    .is_some_and(|m| c.text(m) == "new")
            })
            .and_then(|call| call.child_by_field_name("receiver"))
            .and_then(|receiver| path_segments(receiver, c.src))
            .and_then(|segments| segments.last().cloned());
        match constructed {
            Some(ty) => c.declare_typed(&name, Some(ty), true, false),
            None => c.declare(&name, false),
        }
        if let Some(right) = right {
            match right.kind() {
                "lambda" => c.name_closure(right, &name),
                "call" => {
                    let is_lambda = right
                        .child_by_field_name("method")
                        .is_some_and(|m| matches!(c.text(m), "lambda" | "proc"));
                    if let (true, Some(block)) = (is_lambda, right.child_by_field_name("block")) {
                        c.name_closure(block, &name);
                    }
                }
                _ => {}
            }
        }
    }

    fn call(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(method) = node.child_by_field_name("method") else {
            return;
        };
        let name = c.text(method).to_owned();
        let receiver_node = node.child_by_field_name("receiver");
        if receiver_node.is_none() && matches!(name.as_str(), "require_relative" | "require") {
            self.require(node, &name, c);
            return;
        }
        if receiver_node.is_none() && DECLARATIVE.contains(&name.as_str()) {
            return;
        }
        let range = (method.start_byte(), method.end_byte());
        match receiver_node {
            None => c.call(CallSpec {
                name,
                kind: CallKind::Function,
                receiver: Receiver::None,
                range,
            }),
            Some(receiver) => {
                let constant = matches!(receiver.kind(), "constant" | "scope_resolution");
                if name == "new" && constant {
                    let segments = path_segments(receiver, c.src).unwrap_or_default();
                    let Some((last, prefix)) = segments.split_last() else {
                        return dynamic_call(c, receiver);
                    };
                    let leaf = match receiver.kind() {
                        "scope_resolution" => {
                            receiver.child_by_field_name("name").unwrap_or(receiver)
                        }
                        _ => receiver,
                    };
                    c.call(CallSpec {
                        name: last.clone(),
                        kind: CallKind::Constructor,
                        receiver: if prefix.is_empty() {
                            Receiver::None
                        } else {
                            Receiver::Path(prefix.to_vec())
                        },
                        range: (leaf.start_byte(), leaf.end_byte()),
                    });
                } else {
                    c.call(CallSpec {
                        name,
                        kind: CallKind::Method,
                        receiver: receiver_from(receiver, c.src),
                        range,
                    });
                }
            }
        }
    }

    fn require(&self, node: Node<'_>, name: &str, c: &mut Collector<'_>) {
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let Some(first) = arguments.named_child(0) else {
            return;
        };
        if first.kind() != "string" {
            return;
        }
        let mut cursor = first.walk();
        let content = first
            .named_children(&mut cursor)
            .find(|part| part.kind() == "string_content");
        let Some(content) = content else { return };
        let module = c.text(content).to_owned();
        c.import(ImportFact {
            local: "*".into(),
            imported: "*".into(),
            module: if name == "require_relative" {
                format!("./{module}")
            } else {
                module
            },
            kind: ImportKind::Include,
            type_only: false,
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
        });
    }

    /// A bare identifier that is not a known local is an implicit-self method call.
    fn bare_identifier(&self, node: Node<'_>, c: &mut Collector<'_>) {
        let Some(parent) = node.parent() else { return };
        let is_field = |field: &str| {
            parent
                .child_by_field_name(field)
                .is_some_and(|child| child.id() == node.id())
        };
        let skip = match parent.kind() {
            "call" => is_field("method"),
            "method" | "singleton_method" => is_field("name"),
            "assignment" | "operator_assignment" => is_field("left"),
            "pair" => is_field("key"),
            "for" => is_field("pattern"),
            "method_parameters"
            | "block_parameters"
            | "lambda_parameters"
            | "optional_parameter"
            | "keyword_parameter"
            | "splat_parameter"
            | "hash_splat_parameter"
            | "block_parameter"
            | "destructured_parameter"
            | "left_assignment_list"
            | "rest_assignment"
            | "destructured_left_assignment"
            | "exceptions"
            | "exception_variable"
            | "alias"
            | "undef"
            | "symbol"
            | "simple_symbol"
            | "scope_resolution" => true,
            _ => false,
        };
        if skip {
            return;
        }
        let name = c.text(node).to_owned();
        if c.is_declared(&name) || DECLARATIVE.contains(&name.as_str()) {
            return;
        }
        c.call(CallSpec {
            name,
            kind: CallKind::Function,
            receiver: Receiver::None,
            range: (node.start_byte(), node.end_byte()),
        });
    }
}

// ----- resolution -----

pub(crate) struct RubyScheme;

fn resolve_require(ctx: &ResolutionContext<'_>, from: &str, module: &str) -> Option<String> {
    let name = if module.ends_with(".rb") {
        module.to_owned()
    } else {
        format!("{module}.rb")
    };
    let candidates = if let Some(relative) = name.strip_prefix("./") {
        vec![join_normalized(dirname(from), relative)]
    } else {
        vec![join_normalized("", &name), join_normalized("lib", &name)]
    };
    candidates
        .into_iter()
        .flatten()
        .find(|candidate| ctx.project.contains(candidate))
}

impl Scheme for RubyScheme {
    fn language(&self) -> LanguageId {
        LanguageId::Ruby
    }

    fn bare_call_sees_enclosing_type(&self) -> bool {
        true
    }

    fn types_may_reopen(&self) -> bool {
        true
    }

    fn peers(&self, ctx: &ResolutionContext<'_>) -> Vec<Peer> {
        let mut seen = vec![ctx.path.to_owned()];
        let mut frontier = vec![ctx.path.to_owned()];
        let mut peers = Vec::new();
        for _ in 0..4 {
            let mut next = Vec::new();
            for path in &frontier {
                let Some(file) = ctx.project.file(path) else {
                    continue;
                };
                for import in file
                    .imports
                    .iter()
                    .filter(|i| i.kind == ImportKind::Include)
                {
                    if let Some(target) = resolve_require(ctx, path, &import.module) {
                        if seen.contains(&target) {
                            continue;
                        }
                        seen.push(target.clone());
                        peers.push(Peer {
                            path: target.clone(),
                            namespace: None,
                            functions: true,
                            rule: "require",
                            span: (path == ctx.path)
                                .then(|| project_span(ctx.path, import.start, import.end)),
                        });
                        next.push(target);
                    }
                }
            }
            frontier = next;
        }
        peers
    }
}
