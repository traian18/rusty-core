use harness_code_health::{Category, Severity};
use tree_sitter::Node;

use crate::rule::{finding, rule};

fn walk<'t>(node: Node<'t>, f: &mut impl FnMut(Node<'t>)) {
    f(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, f);
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

rule!(
    MissingStrictMode,
    "sh_missing_strict_mode",
    Category::Infrastructure,
    "A script does not stop on errors or unset variables.",
    [Shell],
    |this, file, config| {
        let Some(tree) = &file.shell else {
            return Vec::new();
        };
        let min_lines = config.limit("sh_missing_strict_mode", "min_lines", 8);
        if file.line_count() < min_lines {
            return Vec::new();
        }
        let mut strict = false;
        walk(tree.root_node(), &mut |node| {
            if node.kind() == "command" {
                let command = text(node, file.text);
                if command.starts_with("set ")
                    && (command.contains("-e")
                        || command.contains("errexit")
                        || command.contains("-euo"))
                {
                    strict = true;
                }
            }
        });
        if strict {
            return Vec::new();
        }
        vec![finding(
            this,
            file,
            Severity::Low,
            (1, 1),
            "",
            "The script never runs `set -euo pipefail`, so a failing command does not stop it."
                .to_owned(),
        )]
    }
);

rule!(
    CurlPipeShell,
    "sh_curl_pipe_sh",
    Category::Security,
    "A script downloads code and pipes it straight into a shell.",
    [Shell],
    |this, file, _config| {
        let Some(tree) = &file.shell else {
            return Vec::new();
        };
        let mut out = Vec::new();
        walk(tree.root_node(), &mut |node| {
            if node.kind() != "pipeline" {
                return;
            }
            let mut cursor = node.walk();
            let commands: Vec<&str> = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "command")
                .map(|c| text(c, file.text))
                .collect();
            let fetches = commands
                .iter()
                .any(|c| c.starts_with("curl ") || c.starts_with("wget "));
            let runs = commands.last().is_some_and(|c| {
                let first = c.split_whitespace().next().unwrap_or("");
                matches!(first, "sh" | "bash" | "zsh" | "sudo")
            });
            if fetches && runs {
                let line = node.start_position().row as u32 + 1;
                out.push(finding(
                    this,
                    file,
                    Severity::Medium,
                    (line, node.end_position().row as u32 + 1),
                    "",
                    "Downloaded code is piped into a shell without being checked; download it, verify a checksum, then run it.".to_owned(),
                ));
            }
        });
        out
    }
);

rule!(
    UnquotedExpansion,
    "sh_unquoted_expansion",
    Category::Infrastructure,
    "A variable is expanded without quotes, so spaces and globs in its value split it.",
    [Shell],
    |this, file, config| {
        let Some(tree) = &file.shell else {
            return Vec::new();
        };
        let limit = config.limit("sh_unquoted_expansion", "max_findings", 10) as usize;
        let mut lines = Vec::new();
        walk(tree.root_node(), &mut |node| {
            if node.kind() != "command" {
                return;
            }
            let mut cursor = node.walk();
            for argument in node.children_by_field_name("argument", &mut cursor) {
                if matches!(argument.kind(), "simple_expansion" | "expansion") {
                    let text = text(argument, file.text);
                    // `$?`, `$#`, `$$` and friends are single words by construction.
                    if text.len() > 2 {
                        lines.push(argument.start_position().row as u32 + 1);
                    }
                }
            }
        });
        lines.dedup();
        lines
            .into_iter()
            .take(limit)
            .map(|line| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (line, line),
                    "",
                    "An unquoted `$variable` is split on spaces and expanded as a glob; write `\"$variable\"`.".to_owned(),
                )
            })
            .collect()
    }
);
