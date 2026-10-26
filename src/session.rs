//! Where the session token lives, for now.
//!
//! **This is a placeholder, and deliberately a small one.** `mm-config` owns
//! configuration and `mm-desktop-xdg` owns secret storage via the Secret
//! Service; neither is written yet. Until they are, a mode-0600 file is honest
//! about what it is rather than pretending to be secure.
//!
//! The file is *not* `~/.config/Mattermost/config.json`: sharing that with the
//! Electron client risks losing its configuration, since two processes would
//! be writing a file only one of them validates.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Session {
    pub(crate) server: String,
    pub(crate) token: String,
    pub(crate) user_id: String,
    pub(crate) username: String,
}

fn path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .ok_or("neither XDG_STATE_HOME nor HOME is set")?;
    Ok(base.join("mattermost-rs").join("session.json"))
}

pub(crate) fn save(session: &Session) -> Result<(), String> {
    let file = path()?;
    let dir = file
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", file.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    let json = serde_json::to_string_pretty(session)
        .map_err(|e| format!("could not serialize the session: {e}"))?;

    // Write to a temporary file and rename, so an interrupted write cannot
    // leave a half-written session behind.
    let temp = file.with_extension("json.new");
    std::fs::write(&temp, json).map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    restrict(&temp)?;
    std::fs::rename(&temp, &file)
        .map_err(|e| format!("could not replace {}: {e}", file.display()))?;

    eprintln!(
        "note: the token is stored unencrypted at {} (mode 0600). \
         Secret Service storage arrives with mm-desktop-xdg.",
        file.display()
    );
    Ok(())
}

#[cfg(unix)]
fn restrict(file: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("could not restrict {}: {e}", file.display()))
}

#[cfg(not(unix))]
fn restrict(_file: &std::path::Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn load() -> Result<Session, String> {
    let file = path()?;
    let raw = std::fs::read_to_string(&file).map_err(|_| {
        format!(
            "not logged in (no session at {}).\nRun `mattermost-rs token --server <url> \
             --token <pat>` or `mattermost-rs login --help`.",
            file.display()
        )
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        format!(
            "the session at {} is unreadable ({e}). Delete it and log in again.",
            file.display()
        )
    })
}

pub(crate) fn clear() -> Result<(), String> {
    let file = path()?;
    match std::fs::remove_file(&file) {
        Ok(()) => {
            println!("removed {}", file.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no session to remove");
            Ok(())
        }
        Err(e) => Err(format!("could not remove {}: {e}", file.display())),
    }
}
