//! Shell scripts through the Bash grammar.
use tree_sitter::{Parser, Tree};

pub fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}
