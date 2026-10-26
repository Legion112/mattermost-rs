//! Reactions.

use crate::{PostId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

/// One person's one reaction to one post.
///
/// The server soft-deletes these like everything else, so `delete_at` has to
/// be checked: a removed reaction can arrive in a post's metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub user_id: UserId,
    pub post_id: PostId,
    pub emoji_name: String,
    pub create_at: Timestamp,
    pub delete_at: Timestamp,
}

impl Reaction {
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        !self.delete_at.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from 12.0.1. The payload carries `channel_id` and
    /// `remote_id` too, which are not modelled and must not break the parse.
    #[test]
    fn the_live_payload_parses_and_ignores_what_is_not_modelled() {
        let json = r#"{"user_id":"u1","post_id":"p1","emoji_name":"tada",
                       "create_at":1793033150209,"update_at":1793033150209,
                       "delete_at":0,"remote_id":"","channel_id":"c1"}"#;
        let reaction: Reaction = serde_json::from_str(json).expect("parses");
        assert_eq!(reaction.emoji_name, "tada");
        assert_eq!(reaction.user_id, UserId::new("u1"));
        assert_eq!(reaction.create_at, Timestamp(1_793_033_150_209));
        assert!(!reaction.is_deleted());
    }

    /// A removed reaction can still arrive, and counting it would show a
    /// reaction nobody has.
    #[test]
    fn a_removed_reaction_says_so() {
        let json = r#"{"emoji_name":"tada","delete_at":1793033150209}"#;
        let reaction: Reaction = serde_json::from_str(json).expect("parses");
        assert!(reaction.is_deleted());
    }
}
