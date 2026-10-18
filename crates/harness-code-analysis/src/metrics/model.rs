use serde::{Deserialize, Serialize};

use crate::model::LanguageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    Function,
    /// Declared in a class, `impl` or trait body.
    Method,
    /// A closure, lambda, arrow function or function expression.
    Closure,
}

/// A boolean expression with two or more operators, as written in one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// 1-based line where the expression starts.
    pub line: u32,
    /// `&&` / `||` / `and` / `or` in the whole expression.
    pub operators: u32,
    /// Both kinds of operator appear.
    pub mixed: bool,
    /// A negation (`!`, `not`) appears inside it.
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionMetrics {
    /// The declared name, or for an anonymous function what it is assigned or passed to
    /// (`<anonymous>` when nothing names it).
    pub name: String,
    /// `Class.method` for methods, otherwise the name.
    pub qualified_name: String,
    pub kind: FunctionKind,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    pub nloc: u32,
    pub ccn: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
    pub bumps: u32,
    pub params: u32,
    /// Parameters that declare a type, and how many of those are scalars or strings.
    pub typed_params: u32,
    pub primitive_params: u32,
    /// A constructor (`new`, `constructor`, `__init__`).
    pub constructor: bool,
    /// The signature is set by another declaration: a trait implementation or an override.
    pub inherited_signature: bool,
    /// Boolean expressions with two or more operators, in document order.
    pub conditions: Vec<Condition>,
    /// Index into [`FileMetrics::classes`] for methods.
    pub class: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassMetrics {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub nloc: u32,
    pub method_count: u32,
    pub max_method_ccn: u32,
    /// Distinct instance members the methods read or write (`self.x`, `this.x`).
    pub field_count: u32,
    /// Groups of methods connected by a shared field or a call, counting only groups that touch
    /// instance state: 1 is cohesive, 2 or more splits into unrelated parts, 0 means no method
    /// touches instance state.
    pub lcom4: u32,
    /// Indexes into [`FileMetrics::functions`].
    pub methods: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetrics {
    pub language: LanguageId,
    pub nloc: u32,
    /// The parse had errors; the numbers cover what parsed.
    pub partial: bool,
    /// In document order.
    pub functions: Vec<FunctionMetrics>,
    pub classes: Vec<ClassMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsError {
    Unsupported(String),
    Parse(String),
}

impl std::fmt::Display for MetricsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(language) => write!(f, "metrics are not available for {language}"),
            Self::Parse(message) => write!(f, "could not parse: {message}"),
        }
    }
}

impl std::error::Error for MetricsError {}
