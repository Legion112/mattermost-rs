//! Custom emoji — the ones a server's own people uploaded.
//!
//! A standard shortcode resolves to a glyph from a table compiled into
//! the binary; a custom one resolves to **an image on the server**, and
//! nothing but the server knows the list. A client that does not fetch
//! it shows `:party_parrot:` where everyone else sees a picture, which
//! on a server that leans on its own emoji is most of the reactions.

use crate::{EmojiId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

/// One custom emoji, as `GET /api/v4/emoji` lists it.
///
/// The `id` is what the image URL is built from, and the `name` is what
/// a reaction and a typed `:shortcode:` carry -- so every lookup goes
/// name to id, never the other way round.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEmoji {
    pub id: EmojiId,
    /// Without colons, and unique on the server.
    #[serde(deserialize_with = "crate::wire::null_default")]
    pub name: String,
    pub creator_id: UserId,
    pub create_at: Timestamp,
    pub delete_at: Timestamp,
}

impl CustomEmoji {
    /// A deleted emoji still comes back in a list the client may have
    /// cached, and drawing it would be a broken image where a shortcode
    /// at least says what was meant.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        !self.delete_at.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a 12.0.1 server. The payload carries `update_at`
    /// too, which is not modelled and must not break the parse.
    #[test]
    fn the_live_payload_parses_and_ignores_what_is_not_modelled() {
        let json = r#"{"id":"e1","create_at":1793033150209,"update_at":1793033150209,
                       "delete_at":0,"creator_id":"u1","name":"party_parrot"}"#;
        let emoji: CustomEmoji = serde_json::from_str(json).expect("parses");
        assert_eq!(emoji.name, "party_parrot");
        assert_eq!(emoji.id, EmojiId::new("e1"));
        assert_eq!(emoji.creator_id, UserId::new("u1"));
        assert_eq!(emoji.create_at, Timestamp(1_793_033_150_209));
        assert!(!emoji.is_deleted());
    }

    /// A removed one can still be in a list this client cached, and
    /// drawing it would be a broken image.
    #[test]
    fn a_removed_emoji_says_so() {
        let json = r#"{"name":"gone","delete_at":1793033150209}"#;
        let emoji: CustomEmoji = serde_json::from_str(json).expect("parses");
        assert!(emoji.is_deleted());
    }

    /// `null` where a string is expected is the trap this whole tree
    /// has been bitten by: `#[serde(default)]` does not cover a key
    /// that is present and null.
    #[test]
    fn a_null_name_is_an_empty_one() {
        let emoji: CustomEmoji = serde_json::from_str(r#"{"name":null}"#).expect("parses");
        assert_eq!(emoji.name, "");
    }
}
