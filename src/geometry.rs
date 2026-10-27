//! Remembering how big the window was.
//!
//! The decision about what is worth restoring is pure and lives in
//! `mm-desktop` at 100%; this is the file on disk.

use mm_desktop::Geometry;
use std::path::PathBuf;

/// Read the size the window had last time, or the default.
pub(crate) fn load() -> Geometry {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<Geometry>(&text).ok())
        .unwrap_or_default()
        .sane()
}

/// Write it down, if it is worth writing down.
pub(crate) fn store(geometry: Geometry) {
    if !geometry.worth_saving() {
        return;
    }
    let Some(path) = path() else { return };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(text) = serde_json::to_string(&geometry) {
        // Best effort: a client that refused to close because it could
        // not write down its own size would be worse than one that
        // opens at the default next time.
        let _ = std::fs::write(path, text);
    }
}

/// Beside the session, under XDG state -- it is state, not config: the
/// user never edits it and losing it costs one window resize.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("mattermost-rs").join("window.json"))
}
