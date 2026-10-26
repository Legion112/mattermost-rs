//! Repository automation.
//!
//! **Outside the coverage gate.** Everything the repository needs doing that is
//! more than a single command lives here, in Rust, rather than in shell
//! scripts: it is compiled, typed, and linted by the same `make check` that
//! guards the rest of the tree. Makefile recipes are one-line delegations to
//! this binary, and CI calls it directly.

mod aptdeps;
mod deps;
mod doctor;
mod package;
mod version;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task> [args]

  check-deps              every member is classified, and no gated crate can
                          reach an I/O crate
  doctor                  check the build environment, especially which GTK
                          pkg-config resolves
  apt-deps                install the system build dependencies (needs sudo)
  package --out <dir>     strip the release binary, smoke-test it, write
                          SHA256SUMS
  version-check --tag vX.Y.Z
                          assert the tag matches the version in Cargo.toml
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(task) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];

    let result = match task.as_str() {
        "check-deps" => deps::run(),
        "doctor" => doctor::run(),
        "apt-deps" => aptdeps::run(),
        "package" => package::run(rest),
        "version-check" => version::run(rest),
        other => Err(format!("unknown task {other:?}\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

/// Pull a `--flag value` pair out of an argument list.
pub(crate) fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}
