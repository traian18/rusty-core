//! Shared tree walk that turns per-language syntax decisions into [`ExtractedFile`] facts.
//!
//! Language modules implement [`Syntax`]; they classify nodes and register bindings, calls and
//! imports through [`Collector`]. Scope handling, ownership and span bookkeeping live here so every
//! language gets the same lexical-ownership behavior.

use std::collections::BTreeSet;

use tree_sitter::{Language, Node, Parser};

use super::facts::*;
use crate::api::CallGraphError;
use crate::model::{CallableKind, Diagnostic, LanguageId};

/// Maximum syntax-tree nesting walked before a subtree is skipped and reported.
const MAX_DEPTH: usize = 400;
/// Parser work budget; parsing aborts (and the file is reported) beyond this.
const PARSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) enum Action {
    /// Visit the children in the current scope.
    Descend,
    /// Ignore this subtree.
    Skip,
    /// This node is a callable; visit its children inside a new callable scope.
    Callable(CallableSpec),
    /// This node is a type or module-like container.
    Type(TypeSpec),
    /// Visit the children with `namespace` in effect.
    Namespace(String),
    /// Visit the children with `name` as the enclosing member container, without declaring a
    /// type (Rust `impl` blocks).
    Container(String),
}

pub(crate) struct CallableSpec {
    pub name: Option<String>,
    /// Byte range of the name; `None` uses the whole node.
    pub name_range: Option<(usize, usize)>,
    pub kind: CallableKind,
    pub container: Option<String>,
    /// Declares the callable in this namespace instead of the one in effect (out-of-line
    /// definitions such as C++ `ns::Foo::bar`).
    pub namespace: Option<String>,
    pub params: Vec<(String, Option<String>)>,
}

pub(crate) struct TypeSpec {
    pub name: String,
    pub name_range: (usize, usize),
    pub kind: TypeKind,
    pub bases: Vec<String>,
}

pub(crate) struct CallSpec {
    pub name: String,
    pub kind: CallKind,
    pub receiver: Receiver,
    pub range: (usize, usize),
}

/// Per-language syntax rules.
pub(crate) trait Syntax: Send + Sync {
    fn language(&self) -> LanguageId;
    fn grammar(&self, path: &str) -> Language;
    fn limitations(&self) -> Vec<String>;
    fn visit(&self, node: Node<'_>, c: &mut Collector<'_>) -> Action;
}

enum Frame {
    Type {
        key: u32,
        name: String,
        start: u32,
        end: u32,
    },
    Callable {
        index: usize,
    },
    Namespace {
        previous: String,
    },
}

pub(crate) struct Collector<'s> {
    pub src: &'s str,
    out: ExtractedFile,
    frames: Vec<Frame>,
    namespace: String,
    pending_closure: Option<(usize, String)>,
    pending_kind: Option<(usize, CallableKind)>,
    used_keys: BTreeSet<u32>,
    module_index: Option<usize>,
    type_inits: Vec<(u32, usize)>,
    truncated_depth: bool,
}

const MODULE_KEY: u32 = u32::MAX;

impl<'s> Collector<'s> {
    fn new(src: &'s str, language: LanguageId) -> Self {
        Self {
            src,
            out: ExtractedFile::empty(language),
            frames: Vec::new(),
            namespace: String::new(),
            pending_closure: None,
            pending_kind: None,
            used_keys: BTreeSet::new(),
            module_index: None,
            type_inits: Vec::new(),
            truncated_depth: false,
        }
    }

    pub fn text(&self, node: Node<'_>) -> &'s str {
        &self.src[node.byte_range()]
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn set_package(&mut self, package: String) {
        self.namespace = package.clone();
        self.out.package = Some(package);
    }

    /// Nearest enclosing type name, if any.
    pub fn current_type(&self) -> Option<&str> {
        self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Type { name, .. } => Some(name.as_str()),
            _ => None,
        })
    }

    /// Whether `name` is a local visible from here. Closures see their enclosing callables'
    /// locals; any other callable is a scope gate.
    pub fn is_declared(&self, name: &str) -> bool {
        for frame in self.frames.iter().rev() {
            if let Frame::Callable { index } = frame {
                let callable = &self.out.callables[*index];
                if callable.locals.iter().any(|local| local.name == name) {
                    return true;
                }
                if callable.kind != CallableKind::Closure {
                    return false;
                }
            }
        }
        self.out
            .module_locals
            .iter()
            .any(|local| local.name == name)
    }

    pub fn in_callable(&self) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, Frame::Callable { .. }))
    }

    /// Names the next closure whose syntax node is `value` after the variable it initializes.
    pub fn name_closure(&mut self, value: Node<'_>, name: &str) {
        self.pending_closure = Some((value.id(), name.to_owned()));
    }

    pub fn import(&mut self, fact: ImportFact) {
        self.out.imports.push(fact);
    }

    pub fn export(&mut self, fact: ExportFact) {
        self.out.exports.push(fact);
    }

    /// Like [`Self::name_closure`] but also fixes the callable kind (for class-field functions).
    pub fn name_closure_as(&mut self, value: Node<'_>, name: &str, kind: CallableKind) {
        self.pending_closure = Some((value.id(), name.to_owned()));
        self.pending_kind = Some((value.id(), kind));
    }

    /// Declares a local value binding in the nearest callable (or at module level).
    pub fn declare(&mut self, name: &str, is_param: bool) {
        self.declare_typed(name, None, false, is_param);
    }

    pub fn declare_typed(&mut self, name: &str, ty: Option<String>, exact: bool, is_param: bool) {
        if name.is_empty() {
            return;
        }
        // Class-body assignments are members, not locals that shadow module-level names.
        if !self.in_callable() && self.current_type().is_some() {
            return;
        }
        let binding = LocalBinding {
            name: name.to_owned(),
            ty,
            ty_exact: exact,
            callable: None,
            is_param,
        };
        self.push_local(binding);
    }

    fn push_local(&mut self, binding: LocalBinding) {
        let index = self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Callable { index } => Some(*index),
            _ => None,
        });
        match index {
            Some(index) => {
                let locals = &mut self.out.callables[index].locals;
                if let Some(existing) = locals.iter_mut().find(|item| item.name == binding.name) {
                    if existing.ty.is_none() {
                        existing.ty = binding.ty;
                        existing.ty_exact = binding.ty_exact;
                    }
                    if binding.callable.is_some() {
                        existing.callable = binding.callable;
                    }
                } else {
                    locals.push(binding);
                }
            }
            None => self.out.module_locals.push(binding),
        }
    }

    fn nearest_callable_key(&self) -> Option<u32> {
        self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Callable { index } => Some(self.out.callables[*index].key),
            _ => None,
        })
    }

    fn unique_key(&mut self, mut key: u32) -> u32 {
        while !self.used_keys.insert(key) {
            key = key.wrapping_add(1);
        }
        key
    }

    pub fn call(&mut self, spec: CallSpec) {
        let caller = self.owner_for_call();
        self.out.calls.push(CallFact {
            caller,
            name: spec.name,
            kind: spec.kind,
            receiver: spec.receiver,
            start: spec.range.0 as u32,
            end: spec.range.1 as u32,
        });
    }

    fn owner_for_call(&mut self) -> u32 {
        if let Some(key) = self.nearest_callable_key() {
            return key;
        }
        let type_frame = self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Type {
                key,
                name,
                start,
                end,
            } => Some((*key, name.clone(), *start, *end)),
            _ => None,
        });
        if let Some((key, name, start, end)) = type_frame {
            if let Some((_, _)) = self.type_inits.iter().find(|(known, _)| *known == key) {
                return key;
            }
            let synthetic_key = self.unique_key(key);
            self.out.callables.push(CallableFact {
                key: synthetic_key,
                name: format!("<init:{name}>"),
                kind: CallableKind::Synthetic,
                container: Some(name),
                namespace: self.namespace.clone(),
                parent: None,
                start,
                end,
                node_start: start,
                node_end: end,
                locals: Vec::new(),
            });
            self.type_inits.push((key, self.out.callables.len() - 1));
            return synthetic_key;
        }
        if self.module_index.is_none() {
            self.out.callables.push(CallableFact {
                key: MODULE_KEY,
                name: "<module>".into(),
                kind: CallableKind::Synthetic,
                container: None,
                namespace: self.namespace.clone(),
                parent: None,
                start: 0,
                end: self.src.len() as u32,
                node_start: 0,
                node_end: self.src.len() as u32,
                locals: Vec::new(),
            });
            self.module_index = Some(self.out.callables.len() - 1);
        }
        MODULE_KEY
    }

    fn enter_callable(&mut self, node: Node<'_>, spec: CallableSpec) {
        let parent = self.nearest_callable_key();
        let node_start = node.start_byte();
        let mut name = spec.name.filter(|name| !name.is_empty());
        let mut named_by_variable = false;
        if let Some((id, pending)) = self.pending_closure.take() {
            if id == node.id() {
                if name.is_none() {
                    name = Some(pending);
                    named_by_variable = true;
                }
            } else {
                self.pending_closure = Some((id, pending));
            }
        }
        let (start, end) = match (&name, spec.name_range) {
            (Some(_), Some(range)) if !named_by_variable => range,
            _ => (node_start, node.end_byte()),
        };
        let name = name.unwrap_or_else(|| format!("<closure@{node_start}>"));
        let mut kind = spec.kind;
        if let Some((id, forced)) = self.pending_kind.take() {
            if id == node.id() {
                kind = forced;
            } else {
                self.pending_kind = Some((id, forced));
            }
        }
        let directly_in_type = matches!(self.frames.last(), Some(Frame::Type { .. }));
        let key = self.unique_key(node_start as u32);
        let container = spec
            .container
            .or_else(|| self.current_type().map(str::to_owned));
        let nested_function = parent.is_some() && matches!(kind, CallableKind::Function);
        let locals = spec
            .params
            .into_iter()
            .map(|(name, ty)| LocalBinding {
                name,
                ty,
                ty_exact: false,
                callable: None,
                is_param: true,
            })
            .collect();
        self.out.callables.push(CallableFact {
            key,
            name: name.clone(),
            kind,
            container,
            namespace: spec.namespace.unwrap_or_else(|| self.namespace.clone()),
            parent,
            start: start as u32,
            end: end as u32,
            node_start: node_start as u32,
            node_end: node.end_byte() as u32,
            locals,
        });
        if (named_by_variable && !directly_in_type) || nested_function {
            self.push_local(LocalBinding {
                name,
                ty: None,
                ty_exact: false,
                callable: Some(key),
                is_param: false,
            });
        }
        let index = self.out.callables.len() - 1;
        self.frames.push(Frame::Callable { index });
    }

    fn enter_type(&mut self, node: Node<'_>, spec: TypeSpec) {
        let key = node.start_byte() as u32;
        self.out.types.push(TypeFact {
            key,
            name: spec.name.clone(),
            kind: spec.kind,
            namespace: self.namespace.clone(),
            start: spec.name_range.0 as u32,
            end: spec.name_range.1 as u32,
            bases: spec.bases,
        });
        self.frames.push(Frame::Type {
            key,
            name: spec.name,
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
        });
    }

    fn exit(&mut self) {
        self.frames.pop();
    }

    fn finish(mut self) -> ExtractedFile {
        if self.truncated_depth {
            self.out.diagnostics.push(Diagnostic {
                path: None,
                message: format!(
                    "syntax nesting depth limit ({MAX_DEPTH}) reached; deeper constructs were skipped"
                ),
            });
        }
        self.out
    }
}

pub(crate) fn extract(
    syntax: &dyn Syntax,
    path: &str,
    source: &str,
) -> Result<ExtractedFile, CallGraphError> {
    let mut parser = Parser::new();
    parser
        .set_language(&syntax.grammar(path))
        .map_err(|error| CallGraphError::DependencyFailure(error.to_string()))?;
    let started = std::time::Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        // Returning true halts parsing.
        started.elapsed() > PARSE_BUDGET
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let tree = parser
        .parse_with_options(
            &mut |offset, _| &source.as_bytes()[offset.min(source.len())..],
            None,
            Some(options),
        )
        .ok_or_else(|| {
            CallGraphError::LimitExceeded(format!("parser work budget exceeded for {path}"))
        })?;
    let mut collector = Collector::new(source, syntax.language());
    walk(tree.root_node(), syntax, &mut collector, 0);
    let mut file = collector.finish();
    collect_errors(tree.root_node(), path, &mut file);
    file.diagnostics.sort_by(|a, b| a.message.cmp(&b.message));
    Ok(file)
}

fn walk(node: Node<'_>, syntax: &dyn Syntax, c: &mut Collector<'_>, depth: usize) {
    if depth > MAX_DEPTH {
        c.truncated_depth = true;
        return;
    }
    // Anonymous tokens (keywords, punctuation) never carry facts; error subtrees are reported
    // separately and are not trusted for extraction.
    if node.is_error() || !node.is_named() {
        return;
    }
    match syntax.visit(node, c) {
        Action::Skip => {}
        Action::Descend => walk_children(node, syntax, c, depth),
        Action::Callable(spec) => {
            c.enter_callable(node, spec);
            walk_children(node, syntax, c, depth);
            c.exit();
        }
        Action::Type(spec) => {
            c.enter_type(node, spec);
            walk_children(node, syntax, c, depth);
            c.exit();
        }
        Action::Container(name) => {
            c.frames.push(Frame::Type {
                key: node.start_byte() as u32,
                name,
                start: node.start_byte() as u32,
                end: node.end_byte() as u32,
            });
            walk_children(node, syntax, c, depth);
            c.exit();
        }
        Action::Namespace(namespace) => {
            let previous = std::mem::replace(&mut c.namespace, namespace);
            c.frames.push(Frame::Namespace { previous });
            walk_children(node, syntax, c, depth);
            if let Some(Frame::Namespace { previous }) = c.frames.pop() {
                c.namespace = previous;
            }
        }
    }
}

fn walk_children(node: Node<'_>, syntax: &dyn Syntax, c: &mut Collector<'_>, depth: usize) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, syntax, c, depth + 1);
    }
}

fn collect_errors(node: Node<'_>, path: &str, file: &mut ExtractedFile) {
    if node.is_error() || node.is_missing() {
        file.error_ranges
            .push((node.start_byte() as u32, node.end_byte() as u32));
        file.diagnostics.push(Diagnostic {
            path: Some(path.to_owned()),
            message: format!(
                "syntax error at bytes {}..{}; facts in this region are partial",
                node.start_byte(),
                node.end_byte()
            ),
        });
        if node.is_error() {
            return;
        }
    }
    if !node.has_error() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, path, file);
    }
}

/// Collects identifier names bound by a parameter or pattern subtree, skipping defaults and types.
pub(crate) fn binding_names(node: Node<'_>, src: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" | "variable_name" | "name" => {
            let text = &src[node.byte_range()];
            if node.kind() == "variable_name" {
                out.push(text.trim_start_matches('$').to_owned());
                return;
            }
            out.push(text.to_owned());
        }
        _ => {
            let mut cursor = node.walk();
            for (index, child) in node.children(&mut cursor).enumerate() {
                let field = node.field_name_for_child(index as u32);
                if matches!(
                    field,
                    Some("value" | "default_value" | "type" | "return_type" | "decorators")
                ) {
                    continue;
                }
                binding_names(child, src, out);
            }
        }
    }
}
