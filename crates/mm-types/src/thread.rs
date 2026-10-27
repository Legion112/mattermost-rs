//! Followed threads — what the Threads view in the sidebar is made of.
//!
//! **Two types wear the word "thread" on this API and they are not the
//! same shape.** `ThreadMembership`, which arrives on the WebSocket, has
//! `following`, `last_view_at` and `unread_mentions`, and no
//! `unread_replies` at all. `ThreadResponse`, which is what the endpoints
//! below return and what this file models, has `unread_replies`,
//! `last_viewed_at` — the other spelling — `post` and `participants`, and
//! no `following`. One struct for both yields silent nulls either way, so
//! there is one struct for one of them.

use crate::{Post, PostId, User};
use serde::{Deserialize, Serialize};

/// One followed thread, as `GET /users/{u}/teams/{t}/threads` returns it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserThread {
    /// The root post's id. Named `id` on the wire, which is why this is
    /// not called `root`: it is the thread's identity and the root post's
    /// identity at once.
    pub id: PostId,
    pub reply_count: i64,
    pub last_reply_at: crate::Timestamp,
    pub last_viewed_at: crate::Timestamp,
    /// Everyone who has posted in the thread. Full users when the request
    /// asked for `extended`, and otherwise objects carrying only an id —
    /// which parses into the same type, with the rest defaulted.
    pub participants: Vec<User>,
    /// The root post. Absent only in payloads that say so; a thread whose
    /// root we cannot draw is skipped rather than drawn empty.
    pub post: Option<Post>,
    pub unread_replies: i64,
    pub unread_mentions: i64,
    pub is_urgent: bool,
    pub delete_at: crate::Timestamp,
}

impl UserThread {
    /// Whether this thread has replies the reader has not seen.
    #[must_use]
    pub fn is_unread(&self) -> bool {
        self.unread_replies > 0 || self.unread_mentions > 0
    }
}

/// The response of `GET /users/{user}/teams/{team}/threads`.
///
/// The totals are the whole account's, not this page's, which is what the
/// sidebar badge needs: asking for one page and counting it would under-
/// report the moment there is more than a page of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadList {
    pub threads: Vec<UserThread>,
    pub total: i64,
    pub total_unread_threads: i64,
    pub total_unread_mentions: i64,
    pub total_unread_urgent_mentions: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped after what 12.x returns, including the fields that are easy
    /// to get the wrong way round: `last_viewed_at` rather than
    /// `last_view_at`, and `unread_replies`, which the membership type
    /// does not have at all.
    const LIVE: &str = r#"{
      "total": 4,
      "total_unread_threads": 2,
      "total_unread_mentions": 1,
      "threads": [
        {"id":"p1","reply_count":8,"last_reply_at":1730000000000,
         "last_viewed_at":1729000000000,"unread_replies":3,"unread_mentions":1,
         "is_urgent":false,
         "participants":[{"id":"u1","username":"alice"},{"id":"u2"}],
         "post":{"id":"p1","message":"the root","user_id":"u1"}}
      ]
    }"#;

    /// The live shape parses, and the totals are the account's rather than
    /// the page's.
    #[test]
    fn the_live_payload_parses() {
        let list: ThreadList = serde_json::from_str(LIVE).expect("parses");
        assert_eq!(list.total, 4);
        assert_eq!(list.total_unread_threads, 2);
        assert_eq!(list.threads.len(), 1);
        let thread = &list.threads[0];
        assert_eq!(thread.id.as_str(), "p1");
        assert_eq!(thread.reply_count, 8);
        assert_eq!(thread.unread_replies, 3);
        assert_eq!(thread.post.as_ref().expect("a root").message, "the root");
    }

    /// Participants arrive as full users or as bare ids depending on
    /// whether the request asked for `extended`, and both have to land in
    /// the same type -- otherwise asking the cheaper way returns a list
    /// of nobody.
    #[test]
    fn participants_parse_whether_or_not_they_are_extended() {
        let list: ThreadList = serde_json::from_str(LIVE).expect("parses");
        let who = &list.threads[0].participants;
        assert_eq!(who[0].username, "alice");
        assert_eq!(who[1].id.as_str(), "u2");
        assert_eq!(who[1].username, "");
    }

    /// A thread is unread on replies or on mentions, and either alone is
    /// enough: a mention with no unread reply count still wants attention.
    #[test]
    fn unread_counts_either_way_round() {
        assert!(!UserThread::default().is_unread());
        assert!(
            UserThread {
                unread_replies: 1,
                ..UserThread::default()
            }
            .is_unread()
        );
        assert!(
            UserThread {
                unread_mentions: 1,
                ..UserThread::default()
            }
            .is_unread()
        );
    }

    /// Round-trip: what we write is what the server would have sent.
    #[test]
    fn a_thread_round_trips() {
        let list: ThreadList = serde_json::from_str(LIVE).expect("parses");
        let text = serde_json::to_string(&list).expect("serializes");
        let back: ThreadList = serde_json::from_str(&text).expect("parses");
        assert_eq!(back, list);
    }

    /// A field the server adds in some later release must not fail the
    /// parse, because that would empty the Threads view on an upgrade.
    #[test]
    fn an_unknown_field_is_ignored() {
        let list: ThreadList =
            serde_json::from_str(r#"{"total":1,"threads":[],"total_unread_sparkles":9}"#)
                .expect("parses");
        assert_eq!(list.total, 1);
    }
}
