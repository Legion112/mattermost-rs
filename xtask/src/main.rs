//! Repository automation.
//!
//! **Outside the coverage gate.** Everything the repository needs doing that is
//! more than a single command lives here, in Rust, rather than in shell
//! scripts: it is compiled, typed, and linted by the same `make check` that
//! guards the rest of the tree. Makefile recipes are one-line delegations to
//! this binary, and CI calls it directly.

mod aptdeps;
mod check;
mod coverage;
mod deb;
mod deps;
mod desktop;
mod doctor;
mod package;
mod parity;
mod version;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task> [args]

  coverage                100% line coverage on the gated crates, or name the gaps
  check-deps              every member is classified, and no gated crate can
                          reach an I/O crate
  doctor                  check the build environment, especially which GTK
                          pkg-config resolves
  apt-deps                install the system build dependencies (needs sudo)
  install-desktop [--autostart]
                          put the icon and the .desktop entry in ~/.local/share,
                          so it appears in the menu and mattermost:// links open
                          it; --autostart also starts it at login, hidden in the
                          tray
  uninstall-desktop       take them away again
  package --out <dir> [--deb]
                          strip the release binary, smoke-test it, write
                          SHA256SUMS; --deb also builds an installable package
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
        "check" => check::run(rest),
        "check-deps" => deps::run(),
        "parity" => parity::run(rest),
        "coverage" => coverage::run(rest),
        "doctor" => doctor::run(),
        "apt-deps" => aptdeps::run(),
        "install-desktop" => desktop::install(rest),
        "uninstall-desktop" => desktop::uninstall(),
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
