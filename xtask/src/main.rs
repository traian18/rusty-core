#![warn(clippy::all)]

//! Workspace checks: `cargo run --manifest-path xtask/Cargo.toml -- <command> [--warn]`.
//!
//! | Command | Checks |
//! |---|---|
//! | `check-deps` | The pure core reaches no I/O, provider, tool or transport |
//! | `check-readmes` | Every crate has the README sections and true dependency lists |
//! | `check-layers` | Engine, intelligence and product crates depend one way only |
//! | `check-all` | All three, in that order; fails if any of them does |
//!
//! Every check fails on any finding. `--warn` prints the findings and succeeds, for reading the
//! debt of a work in progress.
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
        Some("check-all") => run_all(mode),
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

/// Runs every check and fails if any of them did.
fn run_all(mode: Mode) -> ExitCode {
    let results = [
        run_check_deps(),
        run_workspace_check("check-layers", mode, |_, packages| {
            check_layers::check_layers(packages)
        }),
        run_workspace_check("check-readmes", mode, |root, packages| {
            check_readmes::check_readmes(root, packages)
        }),
    ];
    if results.iter().all(|result| *result == ExitCode::SUCCESS) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
    eprintln!("usage: cargo run --manifest-path xtask/Cargo.toml -- <check-all|check-deps|check-readmes|check-layers> [--warn]");
    ExitCode::FAILURE
}
