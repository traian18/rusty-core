//! Checks every Rust file under a repository against its guidelines and prints the debt per crate.
//!
//! `cargo run -p rusty-guidelines --example report -- <repository root>`
//!
//! Violations after the file's first `#[cfg(test)]` line are skipped: inline test modules sit at
//! the end of a file and are allowed to unwrap.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use rusty_code_analysis::{metrics::measure_file, LanguageId};
use rusty_guidelines::{
    check_file, FileInput, Guidelines, Kinds, Violation, GUIDELINES_DIR, LIMITS_RULE, RULES_FILE,
    STYLE_FILE,
};

const SKIPPED_FOLDERS: [&str; 4] = ["target", ".git", "node_modules", "xtask"];

/// Violation counts: crate, then rule.
type Debt = BTreeMap<String, BTreeMap<String, u32>>;

fn main() {
    let root = repository_root_from_args();
    let guidelines = load_guidelines(&root);
    let debt = measure_debt(&root, &guidelines);
    print_debt(&debt);
}

fn repository_root_from_args() -> PathBuf {
    let argument = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    PathBuf::from(argument)
}

fn load_guidelines(root: &Path) -> Guidelines {
    let folder = root.join(GUIDELINES_DIR);
    let rules = fs::read_to_string(folder.join(RULES_FILE)).ok();
    let style = fs::read_to_string(folder.join(STYLE_FILE)).ok();
    Guidelines::parse(rules.as_deref(), style.as_deref()).unwrap_or_else(|error| {
        eprintln!("the guidelines do not parse: {error:?}");
        std::process::exit(1)
    })
}

fn measure_debt(root: &Path, guidelines: &Guidelines) -> Debt {
    let mut debt = Debt::new();
    for file in rust_files_under(root) {
        let path = relative_path(root, &file);
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        for violation in violations_in(guidelines, &path, &source) {
            if wants_details(&path) {
                println!(
                    "{path}:{} [{}] {}",
                    violation.start_line,
                    rule_label(&violation),
                    violation.message
                );
            }
            *debt
                .entry(crate_of(&path))
                .or_default()
                .entry(rule_label(&violation))
                .or_default() += 1;
        }
    }
    debt
}

/// A second argument, a path prefix, lists every violation under it instead of only counting.
fn wants_details(path: &str) -> bool {
    std::env::args()
        .nth(2)
        .is_some_and(|prefix| path.starts_with(&prefix))
}

fn violations_in(guidelines: &Guidelines, path: &str, source: &str) -> Vec<Violation> {
    let metrics = measure_file(&LanguageId::Rust, path, source.as_bytes()).ok();
    let input = FileInput {
        path,
        source,
        metrics: metrics.as_ref(),
        imports: None,
    };
    let inline_tests_start = first_inline_test_line(source);
    check_file(guidelines, &input, Kinds::ALL)
        .into_iter()
        .filter(|violation| violation.start_line < inline_tests_start)
        .collect()
}

/// The rule id, with the size limits told apart: `limits` alone says nothing about which limit.
fn rule_label(violation: &Violation) -> String {
    if violation.rule != LIMITS_RULE {
        return violation.rule.clone();
    }
    let measured = violation.message.split(" has ").nth(1).unwrap_or_default();
    let words: Vec<&str> = measured
        .split(" (the limit")
        .next()
        .unwrap_or_default()
        .split(' ')
        .skip(1)
        .collect();
    let scope = if violation.function.is_some() {
        "function"
    } else {
        "file"
    };
    format!("{scope}: {}", words.join(" "))
}

fn first_inline_test_line(source: &str) -> u32 {
    let position = source
        .lines()
        .position(|line| line.trim_start().starts_with("#[cfg(test)]"));
    position.map_or(u32::MAX, |index| index as u32 + 1)
}

fn rust_files_under(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_skipped(&path) {
            files.extend(rust_files_under(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

fn is_skipped(folder: &Path) -> bool {
    let name = folder.file_name().and_then(|name| name.to_str());
    name.is_some_and(|name| SKIPPED_FOLDERS.contains(&name))
}

fn relative_path(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    relative.to_string_lossy().replace('\\', "/")
}

/// `crates/rusty-agent-core`, `crates/tools/shell`, `apps/harnessd`, ...
fn crate_of(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let depth = match parts.as_slice() {
        ["crates", "tools" | "integrations" | "transports", ..] => 3,
        _ => 2,
    };
    parts[..depth.min(parts.len())].join("/")
}

fn print_debt(debt: &Debt) {
    let mut totals: BTreeMap<&str, u32> = BTreeMap::new();
    for (name, rules) in debt {
        let line: Vec<String> = rules
            .iter()
            .map(|(rule, n)| format!("{rule}={n}"))
            .collect();
        println!("{name}\t{}", line.join(" "));
        for (rule, n) in rules {
            *totals.entry(rule).or_default() += n;
        }
    }
    println!("\nTOTAL");
    for (rule, n) in totals {
        println!("  {rule}\t{n}");
    }
}
