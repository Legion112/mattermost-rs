//! The image cache: avatars, and the images attached to messages.
//!
//! Both are fetched once and kept, because without a cache every launch
//! re-downloads the same faces and the same screenshots. What they are keyed
//! on differs, and the difference is the point:
//!
//! - An **avatar** is keyed on the user id *plus* `last_picture_update`. The
//!   server moves that whenever the picture changes, so invalidation is
//!   automatic and a stale file is impossible rather than merely unlikely.
//! - A **file** is keyed on its id alone. File ids are immutable — a file is
//!   never edited in place, only replaced by a new upload with a new id — so
//!   there is no version to key on and nothing can go stale.

use mm_types::{FileId, Timestamp, UserId};
use std::path::{Path, PathBuf};

/// `$XDG_CACHE_HOME/mattermost-rs`, falling back to `~/.cache`.
///
/// Returns `None` rather than failing: an image that cannot be cached is
/// still an image that can be fetched, so a read-only or unset HOME costs
/// bandwidth and nothing else.
fn cache_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("mattermost-rs"))
}

/// The file for one cache key, under `dir`.
///
/// Split from `cache_root` so the naming rule can be tested without touching
/// the environment — the environment is process-wide and `set_var` is
/// `unsafe`, which this workspace forbids outright.
fn path_in(dir: &Path, key: &str) -> Option<PathBuf> {
    // Ids are base32 and versions are numbers, so a key cannot contain a
    // path separator however odd the server's data is. Filtering rather than
    // escaping, because a key that is entirely punctuation must produce no
    // path at all: writing to the cache directory itself would be worse than
    // not caching.
    let safe: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if safe.trim_matches('_').is_empty() {
        return None;
    }
    Some(dir.join(safe))
}

fn load_from(dir: &Path, key: &str) -> Option<Vec<u8>> {
    std::fs::read(path_in(dir, key)?).ok()
}

fn store_in(dir: &Path, key: &str, bytes: &[u8]) {
    let Some(file) = path_in(dir, key) else {
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

fn avatar_key(user: &UserId, version: Timestamp) -> String {
    format!("{}_{}", user.as_str(), version.millis())
}

/// The cached avatar for this user at this version, if one was written.
#[must_use]
pub(crate) fn load(user: &UserId, version: Timestamp) -> Option<Vec<u8>> {
    load_from(&cache_root()?.join("avatars"), &avatar_key(user, version))
}

/// Write an avatar to the cache, best effort.
pub(crate) fn store(user: &UserId, version: Timestamp, bytes: &[u8]) {
    if let Some(root) = cache_root() {
        store_in(&root.join("avatars"), &avatar_key(user, version), bytes);
    }
}

/// The cached image for one custom emoji.
///
/// Keyed on the emoji's **id**, not its name: a name can be freed and
/// re-used for a different picture, while re-uploading produces a new
/// id -- so the id is the only key that cannot go stale, and it is what
/// the store holds anyway.
#[must_use]
pub(crate) fn load_emoji(emoji: &mm_types::EmojiId) -> Option<Vec<u8>> {
    load_from(&cache_root()?.join("emoji"), emoji.as_str())
}

pub(crate) fn store_emoji(emoji: &mm_types::EmojiId, bytes: &[u8]) {
    if let Some(root) = cache_root() {
        store_in(&root.join("emoji"), emoji.as_str(), bytes);
    }
}

/// The cached copy of an attached file.
#[must_use]
pub(crate) fn load_file(file: &FileId) -> Option<Vec<u8>> {
    load_from(&cache_root()?.join("files"), file.as_str())
}

pub(crate) fn store_file(file: &FileId, bytes: &[u8]) {
    if let Some(root) = cache_root() {
        store_in(&root.join("files"), file.as_str(), bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmrs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A round trip through the cache, keyed by version.
    #[test]
    fn an_avatar_comes_back_under_the_version_it_was_written_with() {
        let dir = scratch("avatars");
        let key = avatar_key(&UserId::new("abc123"), Timestamp(7));

        assert_eq!(load_from(&dir, &key), None);
        store_in(&dir, &key, b"png-bytes");
        assert_eq!(load_from(&dir, &key), Some(b"png-bytes".to_vec()));

        // A new picture moves the version, so the old file is simply never
        // looked at again.
        let moved = avatar_key(&UserId::new("abc123"), Timestamp(8));
        assert_eq!(load_from(&dir, &moved), None);
        // And nothing is left half-written behind it.
        assert!(!dir.join(format!("{key}.part")).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file has no version: its id is immutable, so the key is the id.
    #[test]
    fn a_file_is_keyed_on_its_id_alone() {
        let dir = scratch("files");
        store_in(&dir, "fileid123", b"jpeg-bytes");
        assert_eq!(load_from(&dir, "fileid123"), Some(b"jpeg-bytes".to_vec()));
        assert_eq!(load_from(&dir, "otherfile"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A key with nothing usable in it must not produce a path at all.
    #[test]
    fn a_key_with_no_usable_characters_has_no_path() {
        let dir = PathBuf::from("/tmp");
        assert_eq!(path_in(&dir, "///"), None);
        assert_eq!(path_in(&dir, "_"), None);
        assert_eq!(
            path_in(&dir, "../../etc/passwd"),
            Some(dir.join("etcpasswd"))
        );
        assert_eq!(path_in(&dir, "abc_2"), Some(dir.join("abc_2")));
    }
}
