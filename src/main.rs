//! `mattermost-rs` — a native Linux Mattermost client.
//!
//! **Outside the coverage gate.** This crate is wiring only: it constructs the
//! real transports, starts the engine thread, and hands the GUI its two
//! channels. Every decision it looks like it is making is made in `mm-core`.

use std::process::ExitCode;

const USAGE: &str = "\
usage: mattermost-rs [options]

  --version    print the version and exit
  --help       print this message and exit
";

fn main() -> ExitCode {
    // Real argument parsing arrives with the `login`, `import-electron-config`
    // and `record` subcommands in P0/P1. Until then this is enough to make the
    // build job's `--version` smoke test mean something.
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("mattermost-rs {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument {other:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    eprintln!(
        "mattermost-rs {}: no GUI yet — see docs/parity.md",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::SUCCESS
}
