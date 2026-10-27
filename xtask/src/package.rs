//! `cargo xtask package --out <dir>` — copy the release binary out,
//! strip it, smoke-test it, and write checksums.
//!
//! In Rust rather than a `run: |` block in the workflow so that the packaging
//! steps are the same ones locally and in CI, and so the smoke test can assert
//! something real: that the binary reports the version the manifest claims. A
//! shell block that runs `--version` and discards the output only proves the
//! process exited zero.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = "mattermost-rs";

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let out = PathBuf::from(crate::flag(args, "--out").unwrap_or_else(|| "dist".into()));
    let built = Path::new("target/release").join(BIN);
    if !built.is_file() {
        return Err(format!(
            "{} does not exist — run `cargo build --release --bin {BIN}` first",
            built.display()
        ));
    }

    std::fs::create_dir_all(&out)
        .map_err(|e| format!("could not create {}: {e}", out.display()))?;
    let shipped = out.join(BIN);
    std::fs::copy(&built, &shipped)
        .map_err(|e| format!("could not copy to {}: {e}", shipped.display()))?;

    // Stripped where it will be shipped from, not where it was built.
    // Stripping in place rewrites the file the developer may be running
    // -- `strip` then fails with "Text file busy", which is a confusing
    // way to be told the app is open. Everything below checks the copy,
    // so what is asserted is still exactly what is shipped.
    let status = Command::new("strip")
        .arg(&shipped)
        .status()
        .map_err(|e| format!("could not run strip: {e}"))?;
    if !status.success() {
        return Err(format!("strip failed with {status}"));
    }

    let want = env!("CARGO_PKG_VERSION");
    let reported = Command::new(&shipped)
        .arg("--version")
        .output()
        .map_err(|e| format!("could not run the built binary: {e}"))?;
    if !reported.status.success() {
        return Err(format!(
            "{BIN} --version exited {}:\n{}",
            reported.status,
            String::from_utf8_lossy(&reported.stderr)
        ));
    }
    let line = String::from_utf8_lossy(&reported.stdout).trim().to_owned();
    if !line.contains(want) {
        return Err(format!(
            "{BIN} --version printed {line:?}, which does not contain the manifest version \
             {want:?}. The binary and the manifest have drifted."
        ));
    }
    println!("smoke test: {line}");
    assert_no_homebrew_linkage(&shipped)?;

    // Before the checksums, so the package is in them: a manifest that
    // omits an artefact is worse than no manifest, because it looks
    // complete.
    if args.iter().any(|a| a == "--deb") {
        let deb = crate::deb::build(&out, &shipped, want)?;
        println!("built {}", deb.display());
    }

    // sha256sum-compatible: "<hex>  <name>", two spaces, so `sha256sum -c`
    // can verify it.
    let mut manifest = String::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&out)
        .map_err(|e| format!("could not read {}: {e}", out.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().is_some_and(|n| n != "SHA256SUMS"))
        .collect();
    entries.sort();
    for path in &entries {
        let bytes =
            std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let digest = Sha256::digest(&bytes);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("non-UTF-8 filename in {}", out.display()))?;
        writeln!(manifest, "{digest:x}  {name}")
            .map_err(|e| format!("could not build the checksum manifest: {e}"))?;
    }
    let sums = out.join("SHA256SUMS");
    std::fs::write(&sums, &manifest)
        .map_err(|e| format!("could not write {}: {e}", sums.display()))?;

    print!("{manifest}");
    println!("packaged {} file(s) into {}", entries.len(), out.display());
    Ok(())
}

/// Fail if the shipped binary links anything out of Homebrew's prefix.
///
/// `doctor` guards this at configure time by pinning `PKG_CONFIG_LIBDIR`, but
/// a stray `PKG_CONFIG_PATH` in somebody's shell, or a dependency's build
/// script that resets it, would slip past that and produce a binary which runs
/// on this machine and nowhere else. Checking the artefact itself is the only
/// assertion that cannot be bypassed by the environment.
fn assert_no_homebrew_linkage(bin: &Path) -> Result<(), String> {
    // ldd is not present everywhere (musl images, for one). A missing tool must
    // not fail the build; it just means this check abstains.
    let Ok(out) = Command::new("ldd").arg(bin).output() else {
        println!("linkage: ldd unavailable, check skipped");
        return Ok(());
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let offenders: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("linuxbrew"))
        .map(str::trim)
        .collect();
    if offenders.is_empty() {
        println!(
            "linkage: {} libraries, none from Homebrew",
            text.lines().count()
        );
        return Ok(());
    }
    Err(format!(
        "{} links {} library/libraries from Homebrew's prefix:\n  {}\n\
         Those paths do not exist on another machine or on the CI runner. Run \
         `cargo xtask doctor`, and check that PKG_CONFIG_LIBDIR is still pinned in both the \
         Makefile and .cargo/config.toml.",
        bin.display(),
        offenders.len(),
        offenders.join("\n  ")
    ))
}
