//! `cargo xtask doctor` — check that the build environment is the one we think
//! it is.
//!
//! This exists because of one specific trap. Homebrew installs several hundred
//! `.pc` files into `/home/linuxbrew/.linuxbrew/lib/pkgconfig`, they precede
//! the system ones, and they describe a GTK (4.22) that is not the GTK this
//! machine runs (apt's 4.14 — which is also what the CI runner has). Building
//! against them produces a binary linked to
//! `/home/linuxbrew/.linuxbrew/lib/libgtk-4.so` that matches neither CI nor the
//! `.deb`, and the symptom is a confusing runtime crash rather than a build
//! error.

use std::path::Path;
use std::process::Command;

/// The pkg-config search path that excludes Homebrew. Kept identical to the
/// values in `Makefile` and `.cargo/config.toml`.
const SYSTEM_PKG_CONFIG_LIBDIR: &str = "/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig";

const BREW_PKG_CONFIG: &str = "/home/linuxbrew/.linuxbrew/lib/pkgconfig";

/// Modules we require, with the major.minor series we expect from apt on
/// Ubuntu 24.04. A different series means something other than the apt package
/// is being found.
const REQUIRED: &[(&str, &str)] = &[("gtk4", "4.14"), ("libadwaita-1", "1.5")];

const UNKNOWN: &str = "could not determine";

pub(crate) fn run() -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    println!("toolchain");
    report("rustc", &version_of("rustc", &["--version"]));
    report("cargo", &version_of("cargo", &["--version"]));
    if let Some(v) = first_line(Command::new("cargo").args(["llvm-cov", "--version"])) {
        report("cargo-llvm-cov", &v);
    } else {
        report(
            "cargo-llvm-cov",
            "MISSING — cargo install cargo-llvm-cov (make cover-gate needs it)",
        );
        problems.push("cargo-llvm-cov is not installed".into());
    }

    println!("system libraries (pkg-config, Homebrew excluded)");
    for (module, want) in REQUIRED {
        match pkg_config_version(module) {
            None => {
                report(module, "NOT FOUND — run 'make apt-deps'");
                problems.push(format!("{module} is missing"));
            }
            Some(got) if got == *want || got.starts_with(&format!("{want}.")) => {
                report(module, &got);
            }
            Some(got) => {
                report(
                    module,
                    &format!(
                        "{got} — expected {want}.x; something other than the apt package is on \
                         PKG_CONFIG_LIBDIR"
                    ),
                );
                problems.push(format!("{module} resolves to {got}, not {want}.x"));
            }
        }
    }

    // Report the shadowing explicitly, so a future reader does not have to
    // rediscover why PKG_CONFIG_LIBDIR is pinned.
    if Path::new(BREW_PKG_CONFIG).is_dir() {
        let n = std::fs::read_dir(BREW_PKG_CONFIG)
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "pc"))
                    .count()
            })
            .unwrap_or(0);
        // Probe Homebrew's own .pc files on purpose. Clearing both variables
        // would leave pkg-config with no search path at all and report nothing
        // regardless of what is installed there.
        let brew_gtk = brew_pkg_config_version("gtk4").unwrap_or_else(|| "not installed".into());
        report(
            "homebrew .pc files",
            &format!("{n} present (its gtk4 is {brew_gtk}) — excluded, as intended"),
        );
    }

    println!("lab prerequisites");
    if succeeds("docker", &["info"]) {
        report("docker", &version_of("docker", &["--version"]));
    } else {
        report("docker", "daemon not answering — 'make lab' will not run");
    }

    let targets =
        output(Command::new("rustup").args(["target", "list", "--installed"])).unwrap_or_default();
    if targets.contains("musl") {
        report("musl target", "installed");
    } else {
        report(
            "musl target",
            "MISSING — rustup target add x86_64-unknown-linux-musl",
        );
        problems.push("the musl target is not installed".into());
    }

    println!();
    if problems.is_empty() {
        println!("doctor: environment looks right");
        return Ok(());
    }
    Err(format!(
        "doctor: {} problem(s): {}",
        problems.len(),
        problems.join("; ")
    ))
}

fn report(label: &str, value: &str) {
    println!("  {label:<22} {value}");
}

fn version_of(program: &str, args: &[&str]) -> String {
    first_line(Command::new(program).args(args)).unwrap_or_else(|| UNKNOWN.to_owned())
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `pkg-config --modversion`, with Homebrew excluded.
fn pkg_config_version(module: &str) -> Option<String> {
    first_line(
        Command::new("pkg-config")
            .env("PKG_CONFIG_LIBDIR", SYSTEM_PKG_CONFIG_LIBDIR)
            .env("PKG_CONFIG_PATH", "")
            .args(["--modversion", module]),
    )
}

/// `pkg-config --modversion` against Homebrew's search path only.
fn brew_pkg_config_version(module: &str) -> Option<String> {
    first_line(
        Command::new("pkg-config")
            .env_remove("PKG_CONFIG_LIBDIR")
            .env("PKG_CONFIG_PATH", BREW_PKG_CONFIG)
            .args(["--modversion", module]),
    )
}

fn output(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn first_line(cmd: &mut Command) -> Option<String> {
    let text = output(cmd)?;
    let line = text.lines().next()?.trim().to_owned();
    (!line.is_empty()).then_some(line)
}
