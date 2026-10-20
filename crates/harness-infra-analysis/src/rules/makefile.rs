use std::collections::BTreeSet;

use harness_code_health::{Category, Severity};

use crate::rule::{code, finding, rule};

const COMMAND_TARGETS: [&str; 14] = [
    "all", "clean", "test", "tests", "build", "install", "lint", "check", "run", "dev", "deploy",
    "format", "fmt", "docs",
];

/// `(name, line)` of each plain target rule.
fn targets(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.starts_with('\t') || line.starts_with('#') || line.starts_with(' ') {
            continue;
        }
        let Some((head, rest)) = line.split_once(':') else {
            continue;
        };
        // Variable assignments (`A := b`, `A ?= b`) and special targets are not rules.
        if rest.starts_with('=')
            || head.contains('=')
            || head.starts_with('.')
            || head.contains('%')
        {
            continue;
        }
        for name in head.split_whitespace() {
            out.push((name.to_owned(), index as u32 + 1));
        }
    }
    out
}

fn phony(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix(".PHONY:"))
        .flat_map(|rest| {
            rest.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

rule!(
    MissingPhony,
    "mk_missing_phony",
    Category::Infrastructure,
    "A command-style target is not declared .PHONY, so a file with that name stops it from running.",
    [Makefile],
    |this, file, _config| {
        let declared = phony(file.text);
        targets(file.text)
            .into_iter()
            .filter(|(name, _)| COMMAND_TARGETS.contains(&name.as_str()) && !declared.contains(name))
            .map(|(name, line)| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (line, line),
                    &name,
                    format!("Target {} is a command but is not listed in `.PHONY`.", code(&name)),
                )
            })
            .collect()
    }
);

rule!(
    DuplicateTarget,
    "mk_duplicate_target",
    Category::Infrastructure,
    "A target is defined twice with recipes, and make keeps only the last recipe.",
    [Makefile],
    |this, file, _config| {
        let lines: Vec<&str> = file.text.lines().collect();
        let has_recipe = |line: u32| {
            lines
                .get(line as usize)
                .is_some_and(|l| l.starts_with('\t'))
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (name, line) in targets(file.text) {
            if !has_recipe(line) {
                continue;
            }
            if !seen.insert(name.clone()) {
                out.push(finding(
                    this,
                    file,
                    Severity::Medium,
                    (line, line),
                    &name,
                    format!("Target {} already has a recipe earlier in this file; the earlier one is ignored.", code(&name)),
                ));
            }
        }
        out
    }
);
