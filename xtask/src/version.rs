//! `cargo xtask version-check --tag vX.Y.Z` — assert a release tag matches the
//! manifest.
//!
//! Shipping a `v0.4.0` release containing a 0.3.0 binary is the kind of mistake
//! that is obvious in hindsight and invisible at the time, so the release
//! workflow asserts it before building anything.

use std::process::Command;

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let tag = crate::flag(args, "--tag")
        .ok_or("usage: cargo xtask version-check --tag v0.1.0".to_owned())?;

    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|e| format!("could not run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("could not parse cargo metadata output: {e}"))?;

    let version = json["packages"]
        .as_array()
        .ok_or("cargo metadata: packages is not an array")?
        .iter()
        .find(|p| p["name"].as_str() == Some("mattermost-rs"))
        .and_then(|p| p["version"].as_str())
        .ok_or("could not find the mattermost-rs package in cargo metadata")?;

    let expected = format!("v{version}");
    if tag == expected {
        println!("{tag} matches Cargo.toml");
        Ok(())
    } else {
        Err(format!(
            "tag {tag} does not match Cargo.toml version {expected}. Bump the manifest or retag."
        ))
    }
}
