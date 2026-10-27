//! Remembering how the window was left: its size, and the channel
//! that was open.
//!
//! The decision about what is worth restoring is pure and lives in
//! `mm-desktop` at 100%; this is the file on disk.

use mm_desktop::Restored;
use std::path::PathBuf;

/// Read how the window was left, or the defaults.
pub(crate) fn load() -> Restored {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<Restored>(&text).ok())
        .unwrap_or_default()
        .sane()
}

/// Write it down, if the size is worth writing down.
pub(crate) fn store(state: &Restored) {
    if !state.geometry.worth_saving() {
        return;
    }
    let Some(path) = path() else { return };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(text) = serde_json::to_string(state) {
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
