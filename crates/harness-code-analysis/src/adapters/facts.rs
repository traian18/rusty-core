//! Language-neutral facts produced by [`crate::LanguageAdapter`] implementations.
//!
//! Facts describe one source file only. Cross-file meaning is assigned later by a
//! [`crate::LanguageResolver`]. Offsets are half-open byte ranges into the exact bytes analysed.

use crate::model::{CallableKind, Diagnostic, LanguageId};

/// A name bound inside a callable (parameter or local), used for lexical shadowing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBinding {
    pub name: String,
    /// Best-effort static type name from an annotation or constructor initializer.
    pub ty: Option<String>,
    /// True when `ty` comes from a constructor initializer (the dynamic type is the static one).
    pub ty_exact: bool,
    /// Set when the binding names a nested callable (nested `def`, `const f = () => ..`).
    pub callable: Option<u32>,
    pub is_param: bool,
}

#[derive(Clone, Debug)]
pub struct CallableFact {
    /// Unique within the file; stable for unchanged input.
    pub key: u32,
    pub name: String,
    pub kind: CallableKind,
    /// Lexically enclosing or declared type (class/struct/module) name.
    pub container: Option<String>,
    /// Namespace, package, or inline module path in effect where the callable is declared.
    pub namespace: String,
    /// Enclosing callable key for closures and nested functions.
    pub parent: Option<u32>,
    /// Name span (whole expression for closures and synthetic owners).
    pub start: u32,
    pub end: u32,
    /// Whole definition range, used for error-region overlap.
    pub node_start: u32,
    pub node_end: u32,
    pub locals: Vec<LocalBinding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    /// Bare name call: `f()`.
    Function,
    /// Member syntax: `x.f()`, `x->f()`, `x:f()`.
    Method,
    /// Scope/path syntax: `T::f()`, `pkg.f()` resolved by path.
    Static,
    /// Object construction: `new T()`, `T::new()` is *not* this; `T()` is resolved by name.
    Constructor,
    /// The callee is not a name (computed, variable or call result).
    Dynamic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Receiver {
    None,
    /// `this`, `self`, `$this`, receiver variable in Go.
    SelfValue,
    /// `super`, `parent::`, `base.`.
    Super,
    /// Dotted/scoped path of simple names: `a.b` → `["a", "b"]`.
    Path(Vec<String>),
    /// Any other receiver expression.
    Expr,
}

#[derive(Clone, Debug)]
pub struct CallFact {
    /// Key of the owning callable (possibly a synthetic initialization owner).
    pub caller: u32,
    pub name: String,
    pub kind: CallKind,
    pub receiver: Receiver,
    /// Callee-name span.
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    /// Module-like container: Ruby module, Rust inline `mod`, C++ namespace.
    Module,
}

#[derive(Clone, Debug)]
pub struct TypeFact {
    pub key: u32,
    pub name: String,
    pub kind: TypeKind,
    pub namespace: String,
    pub start: u32,
    pub end: u32,
    pub bases: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportKind {
    /// `import {a as b} from m`, `from m import a as b`, `use m::a as b`.
    Named,
    /// `import a from m`.
    Default,
    /// Binds a whole module/namespace/package to `local`.
    Namespace,
    /// Brings all names of a module/package into scope.
    Wildcard,
    /// `import static T.m`, `using static T`.
    Static,
    /// `use function ..`.
    Function,
    /// Textual include or literal require (`#include "x.h"`, `require_relative 'x'`).
    Include,
}

#[derive(Clone, Debug)]
pub struct ImportFact {
    pub local: String,
    pub imported: String,
    pub module: String,
    pub kind: ImportKind,
    /// Type-only imports never produce runtime calls.
    pub type_only: bool,
    pub start: u32,
    pub end: u32,
}

/// A name a module makes available to importers (`export {a as b}`, `export default f`).
#[derive(Clone, Debug)]
pub struct ExportFact {
    /// Public name; `"default"` for default exports and `"*"` for `export * from`.
    pub exported: String,
    /// Declared name inside the module; empty when the exported value is anonymous.
    pub local: String,
    /// Source module for re-exports.
    pub from: Option<String>,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub struct ExtractedFile {
    pub language: LanguageId,
    /// Package/namespace declared for the file, when the language has one.
    pub package: Option<String>,
    pub callables: Vec<CallableFact>,
    pub calls: Vec<CallFact>,
    pub types: Vec<TypeFact>,
    pub imports: Vec<ImportFact>,
    pub exports: Vec<ExportFact>,
    /// Bindings declared outside any callable.
    pub module_locals: Vec<LocalBinding>,
    /// Byte ranges covered by syntax errors or missing nodes.
    pub error_ranges: Vec<(u32, u32)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ExtractedFile {
    pub fn empty(language: LanguageId) -> Self {
        Self {
            language,
            package: None,
            callables: Vec::new(),
            calls: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            module_locals: Vec::new(),
            error_ranges: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn callable(&self, key: u32) -> Option<&CallableFact> {
        self.callables.iter().find(|item| item.key == key)
    }

    pub fn overlaps_error(&self, start: u32, end: u32) -> bool {
        self.error_ranges
            .iter()
            .any(|(from, to)| *from < end.max(start + 1) && start < (*to).max(*from + 1))
    }
}
