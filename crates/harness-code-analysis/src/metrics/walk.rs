//! One pass over the tree: every function measured on its own body, every class collected with
//! the instance members its methods touch.
use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{
    class::{members, ClassDraft},
    dialect::{Control, Dialect},
    lines::CodeLines,
    model::{ClassMetrics, Condition, FunctionKind, FunctionMetrics},
};

/// Fields whose subtree sits at the depth of the statement itself: conditions, loop headers,
/// the value a `match` inspects, and the `else` of an `if`.
const HEADER_FIELDS: [&str; 9] = [
    "condition",
    "value",
    "subject",
    "left",
    "right",
    "pattern",
    "initializer",
    "increment",
    "parameter",
];

struct Counter<'a> {
    dialect: &'a dyn Dialect,
    source: &'a [u8],
    ccn: u32,
    cognitive: u32,
    max_nesting: u32,
    bumps: u32,
    conditions: Vec<Condition>,
}

impl Counter<'_> {
    fn skip(&self, node: Node<'_>) -> bool {
        self.dialect.function_kind(node).is_some() || self.dialect.is_class(node)
    }

    /// Visits the children of `node` at `depth`; returns the deepest nesting reached.
    fn children(&mut self, node: Node<'_>, depth: u32, in_boolean: bool) -> u32 {
        let mut reached = depth;
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children {
            if !self.skip(child) {
                reached = reached.max(self.visit(child, depth, in_boolean));
            }
        }
        reached
    }

    /// Children of a nesting statement: headers stay at `depth`, bodies go to `inner`.
    fn structured(&mut self, node: Node<'_>, depth: u32, inner: u32, chained: bool) -> u32 {
        let mut reached = inner;
        let mut cursor = node.walk();
        let children: Vec<(Option<&'static str>, Node<'_>)> = node
            .children(&mut cursor)
            .enumerate()
            .map(|(i, child)| (node.field_name_for_child(i as u32), child))
            .collect();
        for (field, child) in children {
            if self.skip(child) {
                continue;
            }
            let at = match field {
                Some(name) if HEADER_FIELDS.contains(&name) => depth,
                Some("alternative") if chained => depth,
                Some(_) => inner,
                None if self.dialect.is_block(child) => inner,
                None => depth,
            };
            reached = reached.max(self.visit(child, at, false));
        }
        reached
    }

    fn negated(&self, node: Node<'_>) -> bool {
        if self.dialect.is_negation(node) {
            return true;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        children
            .into_iter()
            .any(|c| !self.skip(c) && self.negated(c))
    }

    fn boolean_sequence(&self, node: Node<'_>, out: &mut Vec<&'static str>) {
        let operators = self.dialect.boolean_operators(node);
        if operators.is_empty() && !self.dialect.is_transparent(node) {
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "&&" => out.push("&&"),
                "||" => out.push("||"),
                "and" => out.push("and"),
                "or" => out.push("or"),
                _ => self.boolean_sequence(child, out),
            }
        }
    }

    fn visit(&mut self, node: Node<'_>, depth: u32, in_boolean: bool) -> u32 {
        let operators = self.dialect.boolean_operators(node);
        let mut child_in_boolean = in_boolean && self.dialect.is_transparent(node);
        if !operators.is_empty() {
            self.ccn += operators.len() as u32;
            if !in_boolean {
                let mut sequence = Vec::new();
                self.boolean_sequence(node, &mut sequence);
                let operators = sequence.len() as u32;
                let mixed = sequence.windows(2).any(|pair| pair[0] != pair[1]);
                sequence.dedup();
                self.cognitive += sequence.len() as u32;
                if operators >= 2 {
                    self.conditions.push(Condition {
                        line: node.start_position().row as u32 + 1,
                        operators,
                        mixed,
                        negated: self.negated(node),
                    });
                }
            }
            child_in_boolean = true;
        }
        let control = self.dialect.control(node, self.source);
        match control {
            Control::If | Control::Loop | Control::Switch | Control::Catch => {
                if control != Control::Switch {
                    self.ccn += 1;
                }
                self.cognitive += 1 + depth;
                let inner = depth + 1;
                self.max_nesting = self.max_nesting.max(inner);
                let reached = self.structured(node, depth, inner, control == Control::If);
                if depth == 0 && reached >= 2 {
                    self.bumps += 1;
                }
                reached
            }
            Control::ElseIf => {
                self.ccn += 1;
                self.cognitive += 1;
                let inner = depth + 1;
                self.max_nesting = self.max_nesting.max(inner);
                self.structured(node, depth, inner, true)
            }
            Control::Else => {
                self.cognitive += 1;
                let inner = depth + 1;
                self.max_nesting = self.max_nesting.max(inner);
                self.children(node, inner, false)
            }
            Control::Case => {
                self.ccn += 1;
                self.children(node, depth, false)
            }
            Control::Ternary => {
                self.ccn += 1;
                self.cognitive += 1 + depth;
                self.children(node, depth, false)
            }
            Control::Flat => {
                self.ccn += 1;
                self.cognitive += 1;
                self.children(node, depth, false)
            }
            Control::None => self.children(node, depth, child_in_boolean),
        }
    }
}

fn measure(
    node: Node<'_>,
    source: &[u8],
    dialect: &dyn Dialect,
    lines: &CodeLines,
    kind: FunctionKind,
    class: Option<(usize, &str)>,
) -> FunctionMetrics {
    let mut counter = Counter {
        dialect,
        source,
        ccn: 1,
        cognitive: 0,
        max_nesting: 0,
        bumps: 0,
        conditions: Vec::new(),
    };
    if let Some(body) = node.child_by_field_name("body") {
        counter.visit(body, 0, false);
    }
    let name = dialect.function_name(node, source);
    let method = kind == FunctionKind::Method;
    let (typed_params, primitive_params) = dialect.typed_params(node, source);
    let constructor = dialect.is_constructor(node, &name);
    FunctionMetrics {
        qualified_name: match class {
            Some((_, class_name)) if method => format!("{class_name}.{name}"),
            _ => name.clone(),
        },
        name,
        kind,
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        nloc: lines.count(node.start_position().row, node.end_position().row),
        ccn: counter.ccn,
        cognitive: counter.cognitive,
        max_nesting: counter.max_nesting,
        bumps: counter.bumps,
        params: dialect.params(node, source),
        typed_params,
        primitive_params,
        constructor,
        inherited_signature: dialect.inherited_signature(node, source),
        conditions: counter.conditions,
        class: class.filter(|_| method).map(|(index, _)| index),
    }
}

/// Collects every function and class under `node`, in document order.
pub fn collect(
    node: Node<'_>,
    source: &[u8],
    dialect: &dyn Dialect,
    lines: &CodeLines,
    class: Option<usize>,
    functions: &mut Vec<FunctionMetrics>,
    classes: &mut Vec<ClassDraft>,
) {
    let mut class = class;
    if dialect.is_class(node) {
        classes.push(ClassDraft {
            metrics: ClassMetrics {
                name: dialect.class_name(node, source),
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                nloc: lines.count(node.start_position().row, node.end_position().row),
                method_count: 0,
                max_method_ccn: 0,
                field_count: 0,
                lcom4: 0,
                methods: Vec::new(),
            },
            references: Vec::new(),
        });
        class = Some(classes.len() - 1);
    }
    if let Some(kind) = dialect.function_kind(node) {
        let owner = class.filter(|_| kind == FunctionKind::Method);
        let named = owner.map(|index| (index, classes[index].metrics.name.clone()));
        let metrics = measure(
            node,
            source,
            dialect,
            lines,
            kind,
            named.as_ref().map(|(i, n)| (*i, n.as_str())),
        );
        let index = functions.len();
        if let Some(owner) = owner {
            let references: BTreeSet<String> = dialect
                .receiver(node, source)
                .map(|receiver| members(node, source, dialect, &receiver))
                .unwrap_or_default();
            classes[owner].metrics.methods.push(index);
            classes[owner].references.push((index, references));
        }
        functions.push(metrics);
        // Functions nested in a method are not methods of the class.
        class = None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    for child in children {
        collect(child, source, dialect, lines, class, functions, classes);
    }
}
