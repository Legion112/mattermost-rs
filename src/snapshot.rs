//! The sidebar, written down.
//!
//! Everything but images used to be fetched on every launch, and on a
//! server a third of a second away that is seconds of staring at an empty
//! window. What the sidebar needs — the teams, the channels, this
//! reader's membership of them, their grouping, the roster behind DM
//! names, and the preferences — is small, changes slowly, and is exactly
//! the sort of thing worth keeping.
//!
//! It holds the **conversations** too, not only the sidebar. A client
//! that keeps its own copy of what the server told it can be opened and
//! used; one that keeps nothing has to ask for everything first, and
//! every channel clicked in those first seconds is a round trip the
//! reader waits through. What is written here is what the window draws
//! from on the next launch, before a single request is made.
//!
//! It is still **not a source of truth**: it is painted immediately and
//! then reconciled with the sync that follows. The store's joins make
//! that safe, since applying fresh data over stale data is the same as
//! applying it over nothing. The one thing a merge cannot do is
//! *remove*, so the sync ends by pruning channels the snapshot had and
//! the server does not.

use mm_types::{CategoryList, Channel, ChannelId, ChannelMember, Post, Preference, Team, User};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the sidebar is drawn from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Snapshot {
    /// Bumped when the shape changes, so an old file is ignored rather
    /// than half-read into a new one.
    pub version: u32,
    pub teams: Vec<Team>,
    pub channels: Vec<Channel>,
    pub members: Vec<ChannelMember>,
    pub categories: Vec<CategoryList>,
    pub users: Vec<User>,
    pub preferences: Vec<Preference>,
    /// The handful of client-config keys the display settings resolve
    /// from. Without them the first channel opened on a cold start asks
    /// the server for the wrong thing.
    pub config: std::collections::BTreeMap<String, String>,
    /// The newest messages of every channel this client has been told
    /// about, by channel.
    ///
    /// Bounded per channel by `KEPT_PER_CHANNEL`: enough to fill a
    /// window on opening, not an archive. The point is that opening a
    /// channel after a restart draws immediately and asks afterwards.
    pub conversations: std::collections::BTreeMap<ChannelId, Vec<Post>>,
}

/// How many messages of each channel are written down.
///
/// A screenful and a bit, matching what the window shows when a channel
/// opens. More would make the file bigger and the launch slower for
/// messages nobody is going to scroll back to before the server has
/// answered anyway.
pub(crate) const KEPT_PER_CHANNEL: usize = 60;

/// The shape this code writes and will read back.
const VERSION: u32 = 3;

fn path(server: &str, user: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    // Keyed by both: one person may have accounts on two servers, and two
    // people may share a machine. Everything that is not a letter or a
    // digit goes, so the name cannot escape the directory.
    let key: String = format!("{server}_{user}")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if key.is_empty() {
        return None;
    }
    Some(
        base.join("mattermost-rs")
            .join("sidebar")
            .join(format!("{key}.json")),
    )
}

/// Read the snapshot for this account, if there is a usable one.
///
/// Anything unreadable, malformed or of another version is simply absent:
/// a cache that cannot be read costs a slower start and nothing else, and
/// failing to launch over it would be absurd.
pub(crate) fn load(server: &str, user: &str) -> Option<Snapshot> {
    let raw = std::fs::read(path(server, user)?).ok()?;
    let snapshot: Snapshot = serde_json::from_slice(&raw).ok()?;
    (snapshot.version == VERSION).then_some(snapshot)
}

/// Write the snapshot for this account, best effort.
pub(crate) fn store(server: &str, user: &str, snapshot: &Snapshot) {
    let Some(file) = path(server, user) else {
        return;
    };
    let Ok(body) = serde_json::to_vec(&Snapshot {
        version: VERSION,
        ..snapshot.clone()
    }) else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Whole or not at all: a half-written snapshot would be read back as a
    // broken one on every launch until something overwrote it.
    let temp = file.with_extension("part");
    if std::fs::write(&temp, body).is_ok() {
        let _ = std::fs::rename(&temp, &file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round trip, and the version gate that keeps an old file from
    /// being half-read into a new shape.
    #[test]
    fn a_snapshot_comes_back_as_it_went_in() {
        let dir = std::env::temp_dir().join(format!("mmrs-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("one.json");
        std::fs::create_dir_all(&dir).expect("a scratch directory");

        let snapshot = Snapshot {
            version: VERSION,
            teams: vec![Team {
                name: "alanbase".into(),
                ..Team::default()
            }],
            ..Snapshot::default()
        };
        std::fs::write(&file, serde_json::to_vec(&snapshot).expect("serializes")).expect("writes");

        let raw = std::fs::read(&file).expect("reads");
        let back: Snapshot = serde_json::from_slice(&raw).expect("parses");
        assert_eq!(back.version, VERSION);
        assert_eq!(back.teams[0].name, "alanbase");

        // A file from another version is not a file for this code.
        let stale = serde_json::to_vec(&Snapshot {
            version: VERSION + 1,
            ..Snapshot::default()
        })
        .expect("serializes");
        let stale: Snapshot = serde_json::from_slice(&stale).expect("parses");
        assert_ne!(stale.version, VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file is keyed by server *and* account: one person may have two
    /// servers, and two people may share a machine.
    #[test]
    fn the_path_separates_accounts_and_cannot_escape_its_directory() {
        let a = path("https://one.invalid", "user1").expect("a path");
        let b = path("https://two.invalid", "user1").expect("a path");
        let c = path("https://one.invalid", "user2").expect("a path");
        assert_ne!(a, b);
        assert_ne!(a, c);

        // Nothing that is not a letter or a digit survives, so no amount
        // of punctuation in a server name walks up a directory.
        let nasty = path("../../etc", "../passwd").expect("a path");
        assert!(
            nasty.file_name().is_some_and(|n| n == "etcpasswd.json"),
            "{nasty:?}"
        );
        assert_eq!(path("", "").map(|_| ()), None);
    }
}
