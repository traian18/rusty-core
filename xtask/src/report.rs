//! How a check reports: findings are printed, and only `--strict` turns them into a failure.

use std::process::ExitCode;

/// One thing a check found wrong, about one package.
pub struct Finding {
    pub package: String,
    pub problem: String,
}

impl Finding {
    pub fn new(package: &str, problem: impl Into<String>) -> Self {
        Self {
            package: package.to_owned(),
            problem: problem.into(),
        }
    }
}

/// Warn mode lets the rewrite make progress crate by crate; strict mode is for CI at the end.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Warn,
    Strict,
}

impl Mode {
    pub fn from_args(args: &[String]) -> Self {
        if args.iter().any(|argument| argument == "--strict") {
            Self::Strict
        } else {
            Self::Warn
        }
    }
}

/// Prints the findings, grouped by package, and decides the exit status.
pub fn finish(check: &str, mut findings: Vec<Finding>, mode: Mode) -> ExitCode {
    findings.sort_by(|a, b| a.package.cmp(&b.package));
    for finding in &findings {
        println!("{}: {}", finding.package, finding.problem);
    }
    println!("{check}: {} finding(s)", findings.len());
    if mode == Mode::Strict && !findings.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
