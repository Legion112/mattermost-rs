//! `mmlab` — bring the Docker lab up, fill it, and run the client at it.
//!
//! Not an `xtask` subcommand, although everything else in this repository's
//! tooling is: `xtask` is deliberately offline and dependency-light so that
//! `make check` works on a train, and the lab needs tokio, an HTTP client and
//! a Docker daemon. Keeping it a separate binary is what stops those leaking
//! into the one tool CI runs on every push.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo run -p mm-lab --bin mmlab -- <command>

  up                  start the lab (creating it the first time) and wait
                      until the server answers
  seed                fill a running lab with a team, colleagues, channels
                      and a conversation; safe to run twice
  reset               throw the lab away and build a fresh seeded one
  down [--keep-data]  stop and delete the lab; --keep-data keeps the volumes
  status              what is running, and the URL
  watch [as]          print every WebSocket frame the lab sends, as the
                      named account (default: the admin)
  logs [--lines N]    the server's log, where a failed boot says why
  run [--release] [-- args]
                      run the client against the lab, with its own stored
                      session, so the one you use all day is untouched
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];

    let result = match command.as_str() {
        "up" => mm_lab::stack::up().await,
        "seed" => mm_lab::seed::seed().await,
        "reset" => reset().await,
        "down" => mm_lab::stack::down(rest.iter().any(|a| a == "--keep-data")),
        "status" => mm_lab::stack::status().map(|s| print!("{s}")),
        "watch" => {
            mm_lab::watch::watch(rest.first().map_or(mm_lab::seed::ADMIN, String::as_str)).await
        }
        "logs" => lines(rest).and_then(|n| mm_lab::stack::logs(n).map(|s| print!("{s}"))),
        "run" => run_client(rest),
        other => Err(mm_lab::Error(format!(
            "unknown command {other:?}\n\n{USAGE}"
        ))),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn lines(args: &[String]) -> mm_lab::Result<u32> {
    let Some(idx) = args.iter().position(|a| a == "--lines") else {
        return Ok(80);
    };
    args.get(idx + 1)
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| mm_lab::Error("--lines wants a number".to_owned()))
}

async fn reset() -> mm_lab::Result<()> {
    mm_lab::stack::down(false)?;
    mm_lab::stack::up().await?;
    mm_lab::seed::seed().await
}

/// Run the client against the lab in a profile of its own.
///
/// `MMRS_APP_ID` gives the test window a distinct GTK application id and
/// bracketed title, and the three XDG variables give it a separate stored
/// session, cache and dictionary. Without them a lab run would log the real
/// session out of the server you actually work on, which it did exactly once
/// before this existed.
fn run_client(extra: &[String]) -> mm_lab::Result<()> {
    let home = home();
    for dir in ["state", "cache", "data"] {
        std::fs::create_dir_all(home.join(dir))
            .map_err(|e| mm_lab::Error(format!("cannot create {}: {e}", home.display())))?;
    }
    println!(
        "running the client against {}\n  session kept in {}\n  log in as {} / {}",
        mm_lab::stack::URL,
        home.display(),
        mm_lab::seed::ADMIN,
        mm_lab::seed::PASSWORD
    );

    let mut build: Vec<&str> = vec!["run", "--quiet", "--bin", "mattermost-rs"];
    if extra.iter().any(|a| a == "--release") {
        build.push("--release");
    }
    build.push("--");
    let passthrough: Vec<&String> = extra
        .iter()
        .filter(|a| *a != "--" && *a != "--release")
        .collect();
    let status = Command::new(env!("CARGO"))
        .args(build)
        .args(passthrough)
        .env("MMRS_APP_ID", "mmrs.lab")
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .status()
        .map_err(|e| mm_lab::Error(format!("could not run the client: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(mm_lab::Error(format!("the client exited with {status}")))
    }
}

/// Under `target/`, so `cargo clean` throws the lab's session away too and
/// nothing lands in the developer's real `~/.local/state`.
fn home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(std::path::Path::new("."))
        .join("target/lab-home")
}
