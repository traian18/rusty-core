#![warn(clippy::all)]

//! Workspace checks: `cargo run --manifest-path xtask/Cargo.toml -- <command> [--strict]`.
//!
//! | Command | Checks |
//! |---|---|
//! | `check-deps` | The pure core reaches no I/O, provider, tool or transport |
//! | `check-readmes` | Every crate has the README sections and true dependency lists |
//! | `check-layers` | Engine, intelligence and product crates depend one way only |
//!
//! `check-readmes` and `check-layers` print their findings and succeed; `--strict` makes any
//! finding a failure. They run in warn mode until the rewrite has fixed every crate.
mod check_deps;
mod check_layers;
mod check_readmes;
mod report;
mod workspace;

use std::{env, process::ExitCode};

use report::{finish, Finding, Mode};

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let mode = Mode::from_args(&arguments);
    match arguments.first().map(String::as_str) {
        Some("check-deps") => run_check_deps(),
        Some("check-readmes") => run_workspace_check("check-readmes", mode, |root, packages| {
            check_readmes::check_readmes(root, packages)
        }),
        Some("check-layers") => run_workspace_check("check-layers", mode, |_, packages| {
            check_layers::check_layers(packages)
        }),
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            print_usage()
        }
        None => print_usage(),
    }
}

fn run_check_deps() -> ExitCode {
    match check_deps::check_dependencies() {
        Ok(()) => {
            println!("dependency direction checks passed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dependency direction check failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_workspace_check(
    name: &str,
    mode: Mode,
    check: impl Fn(&std::path::Path, &[workspace::Package]) -> Vec<Finding>,
) -> ExitCode {
    let loaded = workspace::workspace_root()
        .and_then(|root| workspace::load_packages(&root).map(|packages| (root, packages)));
    match loaded {
        Ok((root, packages)) => finish(name, check(&root, &packages), mode),
        Err(error) => {
            eprintln!("{name} could not read the workspace: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() -> ExitCode {
    eprintln!("usage: cargo run --manifest-path xtask/Cargo.toml -- <check-deps|check-readmes|check-layers> [--strict]");
    ExitCode::FAILURE
}
