//! `cargo xtask apt-deps` — install the system build dependencies.
//!
//! One list, in one place, so the CI runner, the README and a fresh
//! workstation cannot drift apart. These are *build* dependencies; what
//! the `.deb` depends on at runtime is a separate, deliberately shorter
//! list in `mm_desktop::control`.
//!
//! The spelling dictionaries are the exception, and are listed here
//! because nothing else would list them: they are data, not a linked
//! library, so `$auto` cannot see them, and the composer simply draws no
//! underlines when they are absent. Any other `hunspell-*` package is
//! picked up automatically -- the window checks against every language
//! it finds a dictionary for.

use std::process::Command;

/// `libgtk-4-dev` pulls in cairo, pango, gdk-pixbuf, graphene, epoxy, vulkan
/// and the X11 and Wayland dev packages, so those are not listed individually.
const PACKAGES: &[(&str, &str)] = &[
    ("build-essential", "cc and friends"),
    (
        "pkg-config",
        "how the gtk4 crate finds the system libraries",
    ),
    ("libgtk-4-dev", "GTK 4.14 headers"),
    ("libadwaita-1-dev", "libadwaita 1.5 headers"),
    (
        "libgtksourceview-5-dev",
        "syntax-highlighted code blocks in messages",
    ),
    (
        "musl-tools",
        "builds the static mm-headless the Docker lab injects",
    ),
    (
        "desktop-file-utils",
        "validates the .desktop entry the package ships",
    ),
    ("hunspell-en-us", "spell-checking the composer in English"),
    ("hunspell-ru", "and in Russian"),
    // The UI tests drive the client's real widgets over the accessibility
    // bus, inside a display server nobody is looking at. None of the three
    // is needed to build or to run the client by hand.
    ("xvfb", "a display for `make ui-test` to run the client in"),
    (
        "at-spi2-core",
        "the accessibility bus the UI tests drive the widgets over",
    ),
    (
        "dbus-daemon",
        "a session bus for the accessibility bus to sit on",
    ),
];

pub(crate) fn run() -> Result<(), String> {
    println!("installing {} packages:", PACKAGES.len());
    for (pkg, why) in PACKAGES {
        println!("  {pkg:<24} {why}");
    }
    println!();

    apt(&["update"])?;
    let mut install: Vec<&str> = vec!["install", "-y", "--no-install-recommends"];
    install.extend(PACKAGES.iter().map(|(p, _)| *p));
    apt(&install)?;

    println!("\ninstalled — run `cargo xtask doctor` to confirm the result");
    Ok(())
}

fn apt(args: &[&str]) -> Result<(), String> {
    let status = Command::new("sudo")
        .arg("apt-get")
        .args(args)
        .status()
        .map_err(|e| format!("could not run sudo apt-get: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "sudo apt-get {} failed with {status}",
            args.first().unwrap_or(&"")
        ))
    }
}
