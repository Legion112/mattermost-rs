//! Identifier newtypes.
//!
//! Every Mattermost id is a 26-character base32 string, which means a
//! `ChannelId` passed where a `PostId` belongs compiles fine, runs fine, and
//! returns the wrong thing. These newtypes make that a type error instead.
//!
//! They are deliberately permissive about *content*: the server is the
//! authority on what a valid id looks like, and a client that rejected an id
//! the server issued would be broken in a worse way than one that accepts an
//! unexpected shape. `is_wellformed` is available for tests and diagnostics,
//! never as a gate on input.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The length the server uses for every id it generates.
pub const ID_LEN: usize = 26;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a string as this id. Accepts anything; see the module note.
            #[must_use]
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Whether this looks like an id the server generated. For tests
            /// and diagnostics only — never reject server input with it.
            #[must_use]
            pub fn is_wellformed(&self) -> bool {
                self.0.len() == ID_LEN && self.0.chars().all(|c| c.is_ascii_alphanumeric())
            }

            /// An empty id. The server uses `""` rather than null for "no
            /// parent" on `root_id`, so this is a real, meaningful value.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(raw: &str) -> Self {
                Self(raw.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(raw: String) -> Self {
                Self(raw)
            }
        }
    };
}

id_type!(
    /// A user.
    UserId
);
id_type!(
    /// A team.
    TeamId
);
id_type!(
    /// A channel, of any type including DMs and group DMs.
    ChannelId
);
id_type!(
    /// A post. Also used for a thread root, since a thread is identified by
    /// its root post.
    PostId
);
id_type!(
    /// An uploaded file.
    FileId
);
id_type!(
    /// A custom emoji.
    EmojiId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_as_str_agree() {
        let id = ChannelId::new("abcdefghijklmnopqrstuvwxyz");
        assert_eq!(id.to_string(), id.as_str());
    }

    #[test]
    fn wellformed_accepts_a_real_server_id() {
        assert!(ChannelId::new("4xp9fdt77pncbef59f4k1qey5w").is_wellformed());
    }

    /// The point of `is_wellformed` is diagnostics, so it must say no to the
    /// shapes that actually show up when something is wrong: truncated ids,
    /// ids with punctuation, and the empty string.
    #[test]
    fn wellformed_rejects_the_shapes_that_signal_a_bug() {
        assert!(!ChannelId::new("short").is_wellformed());
        assert!(!ChannelId::new("4xp9fdt77pncbef59f4k1qey5-").is_wellformed());
        assert!(!ChannelId::default().is_wellformed());
    }

    /// `root_id: ""` is how the server says "this is not a reply", so an empty
    /// id is a value and not an error.
    #[test]
    fn empty_is_a_meaningful_value() {
        assert!(PostId::new("").is_empty());
        assert!(!PostId::new("x").is_empty());
    }

    /// Serialized transparently: an id must appear in JSON as a bare string,
    /// not as `{"0": "..."}`.
    #[test]
    fn serializes_as_a_bare_string() {
        let json = serde_json::to_string(&UserId::new("abc")).unwrap();
        assert_eq!(json, "\"abc\"");
        let back: UserId = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(back, UserId::new("abc"));
    }

    #[test]
    fn every_derive_and_conversion_is_exercised() {
        let a = TeamId::from("t1");
        let b = TeamId::from(String::from("t1"));
        assert_eq!(a, b);
        assert_eq!(a, a.clone());
        assert!(!format!("{a:?}").is_empty());
        assert!(a <= b);
        let mut set = std::collections::HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
        assert_eq!(EmojiId::default(), EmojiId::new(""));
        assert_eq!(FileId::new("f").as_str(), "f");
        assert_eq!(ID_LEN, 26);
    }
}
