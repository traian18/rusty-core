//! What each language's syntax says about failure handling and tests: empty and catch-all
//! handlers, calls that abort instead of handling, ignored results, and which functions are tests.
use tree_sitter::Node;

use crate::model::LanguageId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorSmell {
    EmptyHandler,
    BroadHandler,
    Panic,
    IgnoredResult,
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

fn named(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| !c.kind().contains("comment"))
        .collect()
}

/// A block with nothing in it but comments.
fn is_empty(block: Option<Node<'_>>) -> bool {
    block.is_none_or(|b| named(b).is_empty())
}

/// A Python handler body that does nothing: `pass`, `...`, or a bare string.
fn python_idle(block: Option<Node<'_>>) -> bool {
    block.is_none_or(|b| {
        named(b).iter().all(|s| {
            s.kind() == "pass_statement"
                || (s.kind() == "expression_statement"
                    && named(*s)
                        .iter()
                        .all(|e| matches!(e.kind(), "ellipsis" | "string")))
        })
    })
}

fn contains_kind(node: Node<'_>, kinds: &[&str]) -> bool {
    if kinds.contains(&node.kind()) {
        return true;
    }
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| contains_kind(c, kinds));
    found
}

/// Whether the handler's type is `Exception`/`Throwable`-wide.
fn broad_type(type_text: &str) -> bool {
    let t = type_text.trim();
    matches!(
        t,
        "Exception"
            | "Throwable"
            | "BaseException"
            | "\\Exception"
            | "\\Throwable"
            | "System.Exception"
    )
}

pub fn error_smell(language: &LanguageId, node: Node<'_>, source: &[u8]) -> Option<ErrorSmell> {
    use ErrorSmell::*;
    // Keyword tokens share the kind name of the node they open.
    if !node.is_named() {
        return None;
    }
    match language {
        LanguageId::Rust => match node.kind() {
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                if function.kind() != "field_expression" {
                    return None;
                }
                let field = text(function.child_by_field_name("field")?, source);
                matches!(field, "unwrap" | "expect" | "unwrap_err" | "expect_err").then_some(Panic)
            }
            "macro_invocation" => {
                let name = text(node.child_by_field_name("macro")?, source);
                matches!(name, "panic" | "todo" | "unimplemented").then_some(Panic)
            }
            "let_declaration" => {
                let head = text(node, source).trim_start();
                let discards = head.starts_with("let _ =") || head.starts_with("let _:");
                let value = node.child_by_field_name("value")?;
                (discards && matches!(value.kind(), "call_expression" | "await_expression"))
                    .then_some(IgnoredResult)
            }
            "match_arm" => {
                let pattern = text(node.child_by_field_name("pattern")?, source);
                let value = node.child_by_field_name("value")?;
                let idle = value.kind() == "unit_expression"
                    || (value.kind() == "block" && named(value).is_empty());
                (pattern.starts_with("Err(") && idle).then_some(EmptyHandler)
            }
            _ => None,
        },
        LanguageId::Python => {
            if node.kind() != "except_clause" {
                return None;
            }
            let body = named(node).into_iter().find(|c| c.kind() == "block");
            let kinds: Vec<_> = named(node)
                .into_iter()
                .filter(|c| c.kind() != "block")
                .collect();
            let reraises = body.is_some_and(|b| contains_kind(b, &["raise_statement"]));
            if python_idle(body) {
                return Some(EmptyHandler);
            }
            let bare = kinds.is_empty();
            let wide = kinds
                .iter()
                .any(|k| broad_type(text(*k, source).split(" as ").next().unwrap_or("")));
            ((bare || wide) && !reraises).then_some(BroadHandler)
        }
        LanguageId::JavaScript | LanguageId::TypeScript => match node.kind() {
            "catch_clause" => is_empty(node.child_by_field_name("body")).then_some(EmptyHandler),
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                if function.kind() != "member_expression"
                    || text(function.child_by_field_name("property")?, source) != "catch"
                {
                    return None;
                }
                let handler = node.child_by_field_name("arguments")?.named_child(0)?;
                matches!(
                    handler.kind(),
                    "arrow_function" | "function_expression" | "function"
                )
                .then(|| is_empty(handler.child_by_field_name("body")))
                .filter(|empty| *empty)
                .map(|_| EmptyHandler)
            }
            _ => None,
        },
        LanguageId::Java => match node.kind() {
            "catch_clause" => {
                let body = node.child_by_field_name("body");
                if is_empty(body) {
                    return Some(EmptyHandler);
                }
                let kind = named(node)
                    .into_iter()
                    .find(|c| c.kind() == "catch_formal_parameter")
                    .and_then(|p| named(p).into_iter().find(|c| c.kind() == "catch_type"))
                    .map(|t| text(t, source))
                    .unwrap_or("");
                let wide = kind
                    .split('|')
                    .any(|t| broad_type(t.trim_start_matches("final ").trim()));
                let rethrows = body.is_some_and(|b| contains_kind(b, &["throw_statement"]));
                (wide && !rethrows).then_some(BroadHandler)
            }
            "method_invocation" => (text(node.child_by_field_name("name")?, source)
                == "printStackTrace")
                .then_some(Panic),
            _ => None,
        },
        LanguageId::Kotlin => match node.kind() {
            "catch_block" => {
                let body = named(node).into_iter().find(|c| c.kind() == "block");
                if is_empty(body) {
                    return Some(EmptyHandler);
                }
                let wide = named(node)
                    .iter()
                    .any(|c| c.kind() == "user_type" && broad_type(text(*c, source)));
                let rethrows = body
                    .is_some_and(|b| contains_kind(b, &["throw_expression", "jump_expression"]));
                (wide && !rethrows).then_some(BroadHandler)
            }
            "unary_expression" | "postfix_expression" => text(node, source)
                .trim_end()
                .ends_with("!!")
                .then_some(Panic),
            _ => None,
        },
        LanguageId::CSharp => {
            if node.kind() != "catch_clause" {
                return None;
            }
            let body = node.child_by_field_name("body");
            if is_empty(body) {
                return Some(EmptyHandler);
            }
            let ty = named(node)
                .into_iter()
                .find(|c| c.kind() == "catch_declaration")
                .and_then(|d| d.child_by_field_name("type"))
                .map(|t| text(t, source));
            let wide = ty.is_none_or(broad_type);
            let rethrows =
                body.is_some_and(|b| contains_kind(b, &["throw_statement", "throw_expression"]));
            (wide && !rethrows).then_some(BroadHandler)
        }
        LanguageId::Php => match node.kind() {
            "catch_clause" => {
                let body = node.child_by_field_name("body");
                if is_empty(body) {
                    return Some(EmptyHandler);
                }
                let wide = node.child_by_field_name("type").is_some_and(|t| {
                    t.named_children(&mut t.walk())
                        .any(|n| broad_type(text(n, source)))
                });
                let rethrows = body.is_some_and(|b| contains_kind(b, &["throw_expression"]));
                (wide && !rethrows).then_some(BroadHandler)
            }
            "error_suppression_expression" => Some(Panic),
            _ => None,
        },
        LanguageId::Ruby => match node.kind() {
            "rescue" => {
                if node
                    .child_by_field_name("body")
                    .is_none_or(|b| named(b).is_empty())
                {
                    return Some(EmptyHandler);
                }
                let wide = node
                    .child_by_field_name("exceptions")
                    .is_some_and(|e| text(e, source).split(',').any(|t| t.trim() == "Exception"));
                let rethrows =
                    contains_kind(node, &["raise"]) || text(node, source).contains("raise");
                (wide && !rethrows).then_some(BroadHandler)
            }
            "rescue_modifier" => Some(EmptyHandler),
            _ => None,
        },
        LanguageId::Go => match node.kind() {
            "assignment_statement" => {
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                let blank = named(left).iter().all(|l| text(*l, source) == "_");
                (blank && named(right).iter().any(|r| r.kind() == "call_expression"))
                    .then_some(IgnoredResult)
            }
            "if_statement" => {
                let condition = text(node.child_by_field_name("condition")?, source);
                (condition.contains("err != nil")
                    && is_empty(node.child_by_field_name("consequence")))
                .then_some(EmptyHandler)
            }
            "call_expression" => {
                (text(node.child_by_field_name("function")?, source) == "panic").then_some(Panic)
            }
            _ => None,
        },
        LanguageId::Cpp => {
            if node.kind() != "catch_clause" {
                return None;
            }
            if is_empty(node.child_by_field_name("body")) {
                return Some(EmptyHandler);
            }
            let catch_all = node
                .child_by_field_name("parameters")
                .is_some_and(|p| p.named_child_count() == 0);
            let rethrows = text(node, source).contains("throw");
            (catch_all && !rethrows).then_some(BroadHandler)
        }
    }
}

/// Whether `function` is a test by the language's convention.
pub fn is_test(language: &LanguageId, function: Node<'_>, name: &str, source: &[u8]) -> bool {
    match language {
        LanguageId::Rust => {
            let mut sibling = function.prev_sibling();
            while let Some(s) = sibling.filter(|s| s.kind() == "attribute_item") {
                let attr = text(s, source);
                if attr.contains("test") || attr.contains("rstest") {
                    return true;
                }
                sibling = s.prev_sibling();
            }
            false
        }
        LanguageId::Python => name.starts_with("test"),
        LanguageId::JavaScript | LanguageId::TypeScript => {
            let Some(arguments) = function.parent().filter(|p| p.kind() == "arguments") else {
                return false;
            };
            let Some(call) = arguments.parent() else {
                return false;
            };
            let callee = call
                .child_by_field_name("function")
                .map(|f| text(f, source))
                .unwrap_or("");
            matches!(callee.split('.').next(), Some("it" | "test"))
        }
        LanguageId::Java | LanguageId::Kotlin => {
            let mut cursor = function.walk();
            let found = function
                .children(&mut cursor)
                .filter(|c| c.kind() == "modifiers")
                .any(|m| {
                    let t = text(m, source);
                    t.contains("@Test")
                        || t.contains("@ParameterizedTest")
                        || t.contains("@RepeatedTest")
                });
            found
        }
        LanguageId::CSharp => {
            let mut cursor = function.walk();
            let found = function
                .children(&mut cursor)
                .filter(|c| c.kind() == "attribute_list")
                .any(|a| {
                    let t = text(a, source);
                    ["Test", "Fact", "Theory", "TestMethod"]
                        .iter()
                        .any(|k| t.contains(k))
                });
            found
        }
        LanguageId::Php => name.starts_with("test"),
        LanguageId::Ruby => {
            name.starts_with("test_")
                || function
                    .parent()
                    .filter(|p| p.kind() == "call")
                    .and_then(|c| c.child_by_field_name("method"))
                    .is_some_and(|m| matches!(text(m, source), "it" | "specify" | "example"))
        }
        LanguageId::Go => {
            let params = function
                .child_by_field_name("parameters")
                .map(|p| text(p, source))
                .unwrap_or("");
            params.contains("testing.")
                && (name.starts_with("Test") || name.starts_with("Benchmark"))
        }
        LanguageId::Cpp => name.starts_with("TEST") || name.starts_with("TYPED_TEST"),
    }
}

/// Whether `function` sits in code that exists only for tests: it is a test, or it is inside a
/// `#[cfg(test)]` module (helpers, fixtures, mocks).
pub fn in_test_code(language: &LanguageId, function: Node<'_>, name: &str, source: &[u8]) -> bool {
    if is_test(language, function, name, source) {
        return true;
    }
    if *language != LanguageId::Rust {
        return false;
    }
    let mut current = function.parent();
    while let Some(node) = current {
        if node.kind() == "mod_item" {
            let mut s = node.prev_sibling();
            while let Some(a) = s.filter(|a| a.kind() == "attribute_item") {
                if text(a, source).contains("cfg(test)") {
                    return true;
                }
                s = a.prev_sibling();
            }
        }
        current = node.parent();
    }
    false
}

/// Assertion-like tokens in `text` (case-insensitive).
pub fn count_assertions(text: &str) -> u32 {
    let lower = text.to_ascii_lowercase();
    let mut count = 0;
    for token in [
        "assert",
        "expect(",
        "expect_",
        ".should",
        "should.",
        "verify(",
        "t.error",
        "t.fatal",
        "require.",
        "must(",
        "fail(",
        "panic!(",
        "unreachable!(",
        ".unwrap()",
        ".unwrap_err()",
        ".expect_err(",
    ] {
        count += lower.matches(token).count() as u32;
    }
    // Rust tests that return a Result and use `?` fail on error.
    count + u32::from(text.contains("?;") && lower.contains("-> result"))
}

/// Tests marked ignored or skipped in `source`. Attributes and annotations count where a line
/// starts with them; calls count anywhere on a line that does not have a string or comment
/// before them.
pub fn count_skipped(source: &str) -> u32 {
    const MARKERS: [&str; 8] = [
        "#[ignore",
        "@Ignore",
        "@Disabled",
        "[Ignore",
        "@pytest.mark.skip",
        "@unittest.skip",
        "@Skip",
        "@pending",
    ];
    const CALLS: [&str; 9] = [
        ".skip(",
        "xit(",
        "xdescribe(",
        "xtest(",
        "t.Skip(",
        "t.Skipf(",
        "markTestSkipped",
        "markTestIncomplete",
        "DISABLED_",
    ];
    let mut count = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if MARKERS.iter().any(|m| trimmed.starts_with(m)) {
            count += 1;
            continue;
        }
        for call in CALLS {
            if let Some(at) = line.find(call) {
                let before = &line[..at];
                if !before.contains('"')
                    && !before.contains('`')
                    && !before.contains("//")
                    && !before.contains('#')
                {
                    count += 1;
                    break;
                }
            }
        }
    }
    count
}
