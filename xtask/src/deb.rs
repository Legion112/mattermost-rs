//! `cargo xtask package --deb` — build a `.deb` from the release
//! binary.
//!
//! By hand with `dpkg-deb` rather than through `cargo-deb`: the
//! package is four files and a control stanza, the control stanza is
//! rendered and tested in `mm-desktop`, and a build tool that has to
//! be installed before the build works is one more thing between a
//! clone and an artefact.
//!
//! The binary goes to `/usr/bin`, and the icon and the `.desktop`
//! entry to the system directories -- the same two files `xtask
//! install-desktop` puts in the user's own, which is why the entry is
//! rendered from the same function with a different path in it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where each piece lands inside the package.
const BIN_DIR: &str = "usr/bin";
const ENTRY_DIR: &str = "usr/share/applications";
const ICON_DIR: &str = "usr/share/icons/hicolor/scalable/apps";
/// Where the installed binary will be, which is what the entry's
/// `Exec=` has to say -- not where it is being built from.
const INSTALLED_BIN: &str = "/usr/bin/mattermost-rs";

pub(crate) fn build(out: &Path, binary: &Path, version: &str) -> Result<PathBuf, String> {
    let arch = architecture()?;
    let staging = out.join(format!("mattermost-rs_{version}_{arch}"));
    // From scratch every time: a file left over from a previous build
    // would be shipped without anything noticing.
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| format!("cannot clear {}: {e}", staging.display()))?;
    }

    let shipped_binary = staging.join(BIN_DIR).join("mattermost-rs");
    put(&shipped_binary, &read(binary)?)?;

    let id = mm_desktop::APP_ID;
    put(
        &staging.join(ENTRY_DIR).join(format!("{id}.desktop")),
        mm_desktop::render_desktop_entry(Path::new(INSTALLED_BIN)).as_bytes(),
    )?;
    put(
        &staging.join(ICON_DIR).join(format!("{id}.svg")),
        &read(&repo_root().join("assets").join(format!("{id}.svg")))?,
    )?;

    // Measured after everything is in place, because that is what the
    // field means: the size of what gets installed, in kibibytes.
    let size = installed_size(&staging)?;
    put(
        &staging.join("DEBIAN/control"),
        mm_desktop::render_control(version, &arch, size).as_bytes(),
    )?;

    // Last, because it has to cover everything that was just written:
    // the build machine's umask is nobody's business once the package
    // is on somebody else's disk.
    normalise_modes(&staging)?;
    executable(&shipped_binary)?;

    let deb = out.join(format!("mattermost-rs_{version}_{arch}.deb"));
    let status = Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&staging)
        .arg(&deb)
        .status()
        .map_err(|e| format!("could not run dpkg-deb: {e}"))?;
    if !status.success() {
        return Err(format!("dpkg-deb failed with {status}"));
    }
    // The staging tree has served its purpose and is only confusing
    // next to the artefact.
    let _ = std::fs::remove_dir_all(&staging);
    Ok(deb)
}

/// The Debian architecture name, which is not the one `uname` gives:
/// `x86_64` is `amd64` here, and getting it wrong makes a package
/// that refuses to install with a message about the wrong machine.
fn architecture() -> Result<String, String> {
    let out = Command::new("dpkg")
        .args(["--print-architecture"])
        .output()
        .map_err(|e| format!("could not ask dpkg for the architecture: {e}"))?;
    if !out.status.success() {
        return Err("dpkg --print-architecture failed".to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Total size of what will be installed, in kibibytes, rounded up.
fn installed_size(staging: &Path) -> Result<u64, String> {
    fn walk(dir: &Path, total: &mut u64) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let meta = entry
                .metadata()
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if meta.is_dir() {
                walk(&entry.path(), total)?;
            } else {
                *total += meta.len();
            }
        }
        Ok(())
    }
    let mut bytes = 0;
    walk(staging, &mut bytes)?;
    Ok(bytes.div_ceil(1024))
}

/// 0755 on directories, 0644 on files.
///
/// Whatever umask the build ran under is baked into the package
/// otherwise, so a developer with the common group-writable 002 ships
/// group-writable files into `/usr/share` -- which is a policy
/// violation, and the kind that only shows up in somebody else's
/// lintian run.
fn normalise_modes(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            mode(&path, 0o755)?;
            normalise_modes(&path)?;
        } else {
            mode(&path, 0o644)?;
        }
    }
    mode(dir, 0o755)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn put(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// 0755, or the package installs something nobody can run.
fn executable(path: &Path) -> Result<(), String> {
    mode(path, 0o755)
}

fn mode(path: &Path, bits: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))
        .map_err(|e| format!("cannot set {bits:o} on {}: {e}", path.display()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}
