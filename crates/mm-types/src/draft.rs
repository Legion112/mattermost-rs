//! Unsent messages, as the server keeps them.
//!
//! Mattermost stores drafts server-side so they follow the user between
//! clients: a message half-typed in the web app is waiting in the desktop
//! one. That is why the Drafts view can be drawn at all before this client
//! writes drafts of its own — the ones it shows were typed elsewhere.

use crate::{ChannelId, FileId, PostId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

/// One unsent message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub create_at: Timestamp,
    pub update_at: Timestamp,
    pub delete_at: Timestamp,
    pub user_id: UserId,
    pub channel_id: ChannelId,
    /// Set when the draft is a reply, in which case it belongs to a
    /// thread rather than to the channel's composer.
    pub root_id: PostId,
    pub message: String,
    pub file_ids: Vec<FileId>,
}

impl Draft {
    /// Whether this draft is a reply waiting in a thread rather than a
    /// message waiting in a channel.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        !self.root_id.as_str().is_empty()
    }

    /// Whether there is anything in it. The server keeps a draft whose
    /// text has been deleted but which still carries files, and drops one
    /// that has neither.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message.trim().is_empty() && self.file_ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped after what 12.x returns for `GET /users/{u}/teams/{t}/drafts`.
    const LIVE: &str = r#"[
      {"create_at":1730000000000,"update_at":1730000001000,"delete_at":0,
       "user_id":"u1","channel_id":"c1","root_id":"","message":"half a thought",
       "file_ids":[],"props":{}},
      {"create_at":1730000002000,"update_at":1730000002000,"delete_at":0,
       "user_id":"u1","channel_id":"c2","root_id":"p9","message":"a reply",
       "file_ids":["f1"]}
    ]"#;

    /// The live shape parses, including `props`, which this type does not
    /// model and must therefore ignore rather than refuse.
    #[test]
    fn the_live_payload_parses() {
        let drafts: Vec<Draft> = serde_json::from_str(LIVE).expect("parses");
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].message, "half a thought");
        assert_eq!(drafts[0].channel_id.as_str(), "c1");
        assert_eq!(drafts[1].file_ids[0].as_str(), "f1");
    }

    /// A draft with a root is a reply and belongs to a thread; one
    /// without belongs to the channel. Drawn differently, so the
    /// distinction has to survive the wire.
    #[test]
    fn a_root_id_makes_a_draft_a_reply() {
        let drafts: Vec<Draft> = serde_json::from_str(LIVE).expect("parses");
        assert!(!drafts[0].is_reply());
        assert!(drafts[1].is_reply());
    }

    /// Empty means nothing to show: no text and no files. Text alone or
    /// files alone is a draft worth listing.
    #[test]
    fn a_draft_is_empty_only_with_neither_text_nor_files() {
        assert!(Draft::default().is_empty());
        assert!(
            Draft {
                message: "   ".into(),
                ..Draft::default()
            }
            .is_empty()
        );
        assert!(
            !Draft {
                message: "a".into(),
                ..Draft::default()
            }
            .is_empty()
        );
        assert!(
            !Draft {
                file_ids: vec![FileId::new("f1")],
                ..Draft::default()
            }
            .is_empty()
        );
    }

    /// Round-trip: what we write is what the server would have sent.
    #[test]
    fn a_draft_round_trips() {
        let drafts: Vec<Draft> = serde_json::from_str(LIVE).expect("parses");
        let text = serde_json::to_string(&drafts).expect("serializes");
        let back: Vec<Draft> = serde_json::from_str(&text).expect("parses");
        assert_eq!(back, drafts);
    }
}
