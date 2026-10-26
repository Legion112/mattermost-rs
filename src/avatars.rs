//! The avatar file cache.
//!
//! Avatars are the one thing this client fetches per *person* rather than
//! per channel, so without a cache every launch re-downloads the same dozen
//! images. The key is the user id plus `last_picture_update`, which the
//! server moves whenever the picture changes — so invalidation is automatic
//! and a stale file is impossible rather than merely unlikely.

use mm_types::{Timestamp, UserId};
use std::path::{Path, PathBuf};

/// `$XDG_CACHE_HOME/mattermost-rs/avatars`, falling back to `~/.cache`.
///
/// Returns `None` rather than failing: an avatar that cannot be cached is
/// still an avatar that can be fetched, so a read-only or unset HOME costs
/// bandwidth and nothing else.
fn directory() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("mattermost-rs").join("avatars"))
}

/// The filename for one user at one version, under `dir`.
///
/// Split from `directory` so the naming rule can be tested without touching
/// the environment — the environment is process-wide and `set_var` is
/// `unsafe`, which this workspace forbids outright.
fn path_in(dir: &Path, user: &UserId, version: Timestamp) -> Option<PathBuf> {
    // The id is 26 base32 characters and the version is a number, so the
    // name cannot contain a path separator however odd the server's data is.
    let name: String = user
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(dir.join(format!("{name}_{}", version.millis())))
}

fn load_from(dir: &Path, user: &UserId, version: Timestamp) -> Option<Vec<u8>> {
    std::fs::read(path_in(dir, user, version)?).ok()
}

fn store_in(dir: &Path, user: &UserId, version: Timestamp, bytes: &[u8]) {
    let Some(file) = path_in(dir, user, version) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    // A partial write would be read back as a corrupt image forever, so the
    // file appears whole or not at all.
    let temp = file.with_extension("part");
    if std::fs::write(&temp, bytes).is_ok() {
        let _ = std::fs::rename(&temp, &file);
    }
}

/// The cached image for this user at this version, if one was written.
#[must_use]
pub(crate) fn load(user: &UserId, version: Timestamp) -> Option<Vec<u8>> {
    load_from(&directory()?, user, version)
}

/// Write an image to the cache, best effort.
pub(crate) fn store(user: &UserId, version: Timestamp, bytes: &[u8]) {
    if let Some(dir) = directory() {
        store_in(&dir, user, version, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round trip through the cache, keyed by version.
    #[test]
    fn an_avatar_comes_back_under_the_version_it_was_written_with() {
        let dir = std::env::temp_dir().join(format!("mmrs-avatars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let user = UserId::new("abc123");

        assert_eq!(load_from(&dir, &user, Timestamp(7)), None);
        store_in(&dir, &user, Timestamp(7), b"png-bytes");
        assert_eq!(
            load_from(&dir, &user, Timestamp(7)),
            Some(b"png-bytes".to_vec())
        );
        // A new picture moves the version, so the old file is simply never
        // looked at again.
        assert_eq!(load_from(&dir, &user, Timestamp(8)), None);
        // And nothing is left half-written behind it.
        assert!(!dir.join("abc123_7.part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An id with nothing usable in it must not produce a path at all --
    /// writing to the cache directory itself would be worse than not caching.
    #[test]
    fn an_empty_id_has_no_cache_path() {
        let dir = PathBuf::from("/tmp");
        assert_eq!(path_in(&dir, &UserId::new("///"), Timestamp(1)), None);
        assert_eq!(
            path_in(&dir, &UserId::new("abc"), Timestamp(2)),
            Some(dir.join("abc_2"))
        );
    }
}
