//! Writing an attachment into the reader's download folder.
//!
//! The decisions -- what to call it, where to put it, what to do when
//! the name is taken -- are pure and live in `mm-desktop` at 100%.
//! This is the part that touches the disk and nothing else.

use std::path::{Path, PathBuf};

/// Save `bytes` under `name`, and say where it landed.
pub(crate) fn save(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let folder = downloads_dir();
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("cannot use {}: {e}", folder.display()))?;
    let path = mm_desktop::destination(&folder, name, std::path::Path::exists);
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Where the desktop keeps downloads.
///
/// `XDG_DOWNLOAD_DIR` first, because somebody who has moved it means
/// it; `~/Downloads` otherwise, which is where it is on a stock
/// install. It is created if it is missing rather than refused: a
/// freshly installed account may not have one yet.
fn downloads_dir() -> PathBuf {
    if let Some(set) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        let path = PathBuf::from(set);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("."),
        |home| Path::new(&home).join("Downloads"),
    )
}
