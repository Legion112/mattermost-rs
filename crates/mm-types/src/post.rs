//! Posts and the paged list the server returns them in.

use crate::{ChannelId, FileId, PostId, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A post.
///
/// `props` and `metadata` are kept as opaque JSON on purpose. They carry
/// attachments, embeds, reactions, priority and whatever the server adds next;
/// modelling them eagerly would mean a breaking change in this crate every
/// time the server grows a feature the client does not use yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Post {
    pub id: PostId,
    pub create_at: Timestamp,
    pub update_at: Timestamp,
    /// Non-zero if the post has been edited.
    pub edit_at: Timestamp,
    /// Non-zero if the post has been deleted. Deletes are soft, and `?since=`
    /// returns deleted posts, so this must be checked on every post that
    /// arrives from that endpoint.
    pub delete_at: Timestamp,
    pub user_id: UserId,
    pub channel_id: ChannelId,
    /// Empty for a root post; the thread root's id for a reply.
    pub root_id: PostId,
    pub original_id: PostId,
    pub message: String,
    /// `""` for an ordinary message; `system_*` for join/leave and the like.
    #[serde(rename = "type")]
    pub kind: String,
    pub hashtags: String,
    /// The client-generated id used to match an optimistic post to the
    /// `posted` event that confirms it.
    pub pending_post_id: String,
    pub reply_count: i64,
    pub is_pinned: bool,
    pub file_ids: Vec<FileId>,
    pub props: serde_json::Value,
    pub metadata: serde_json::Value,
}

impl Post {
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        !self.delete_at.is_zero()
    }

    #[must_use]
    pub fn is_edited(&self) -> bool {
        !self.edit_at.is_zero()
    }

    /// A reply, as opposed to a root post.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        !self.root_id.is_empty()
    }

    /// The thread this post belongs to, which is itself for a root post.
    #[must_use]
    pub fn thread_root(&self) -> &PostId {
        if self.root_id.is_empty() {
            &self.id
        } else {
            &self.root_id
        }
    }

    /// A join/leave/header-change notice rather than something a person typed.
    #[must_use]
    pub fn is_system(&self) -> bool {
        self.kind.starts_with("system_")
    }
}

/// A page of posts.
///
/// **`posts` is a superset of `order`.** The server's `?since=` query unions in
/// the root posts of any changed replies, and those arrive in the map without
/// appearing in the ordering. Iterate `order` to build a timeline; treat
/// `posts` as a lookup table. A client that iterates `posts` will render
/// unrelated root posts into the middle of a channel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostList {
    /// Newest first, for the page/before/after queries.
    pub order: Vec<PostId>,
    pub posts: HashMap<PostId, Post>,
    /// Cursor for paging further back; empty when there is no more.
    pub prev_post_id: String,
    /// Cursor for paging forward; empty when caught up.
    pub next_post_id: String,
    /// Only some endpoints send this.
    pub has_next: Option<bool>,
    /// Non-zero when a retention or cloud limit truncated history.
    pub first_inaccessible_post_time: i64,
}

impl PostList {
    /// The posts named by `order`, in that order, skipping any id the server
    /// listed but did not include.
    pub fn ordered(&self) -> impl Iterator<Item = &Post> {
        self.order.iter().filter_map(|id| self.posts.get(id))
    }

    /// Posts present in the map but absent from `order`.
    ///
    /// These are real and must not be rendered into the timeline, but they are
    /// worth keeping: they are usually thread roots the client needs in order
    /// to attach a changed reply.
    pub fn unordered(&self) -> impl Iterator<Item = &Post> {
        self.posts
            .iter()
            .filter(|(id, _)| !self.order.contains(id))
            .map(|(_, post)| post)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty() && self.posts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str, root: &str) -> Post {
        Post {
            id: PostId::new(id),
            root_id: PostId::new(root),
            ..Post::default()
        }
    }

    #[test]
    fn root_and_reply_are_distinguished_by_root_id() {
        let root = post("p1", "");
        let reply = post("p2", "p1");
        assert!(!root.is_reply());
        assert!(reply.is_reply());
        assert_eq!(root.thread_root(), &PostId::new("p1"));
        assert_eq!(reply.thread_root(), &PostId::new("p1"));
    }

    #[test]
    fn deleted_and_edited_are_nonzero_timestamps() {
        let mut p = post("p1", "");
        assert!(!p.is_deleted());
        assert!(!p.is_edited());
        p.delete_at = Timestamp(5);
        p.edit_at = Timestamp(6);
        assert!(p.is_deleted());
        assert!(p.is_edited());
    }

    #[test]
    fn system_posts_are_recognised_by_their_type_prefix() {
        let mut p = post("p1", "");
        assert!(!p.is_system());
        p.kind = "system_join_channel".into();
        assert!(p.is_system());
        p.kind = "custom_thing".into();
        assert!(!p.is_system());
    }

    /// The defining subtlety of `PostList`: `posts` holds entries that `order`
    /// does not name, and rendering those would put unrelated root posts in
    /// the middle of a channel.
    #[test]
    fn ordered_follows_order_and_ignores_the_extra_posts() {
        let mut list = PostList {
            order: vec![PostId::new("p2"), PostId::new("p1")],
            ..PostList::default()
        };
        for p in ["p1", "p2", "p9"] {
            list.posts.insert(PostId::new(p), post(p, ""));
        }

        let ordered: Vec<&str> = list.ordered().map(|p| p.id.as_str()).collect();
        assert_eq!(ordered, vec!["p2", "p1"], "must follow order, not the map");

        let extra: Vec<&str> = list.unordered().map(|p| p.id.as_str()).collect();
        assert_eq!(extra, vec!["p9"], "the unioned-in root must be reachable");
    }

    /// An id in `order` that the server did not include in `posts` must be
    /// skipped rather than panicking.
    #[test]
    fn ordered_skips_an_id_with_no_post() {
        let list = PostList {
            order: vec![PostId::new("missing")],
            ..PostList::default()
        };
        assert_eq!(list.ordered().count(), 0);
    }

    #[test]
    fn parses_a_realistic_page_and_ignores_unknown_fields() {
        let list: PostList = serde_json::from_str(
            r#"{"order":["p1"],
                "posts":{"p1":{"id":"p1","message":"hi","user_id":"u1",
                               "channel_id":"c1","type":"","create_at":17,
                               "something_new":true}},
                "next_post_id":"","prev_post_id":"p0","has_next":false}"#,
        )
        .unwrap();
        assert_eq!(list.order.len(), 1);
        assert_eq!(list.posts[&PostId::new("p1")].message, "hi");
        assert_eq!(list.prev_post_id, "p0");
        assert_eq!(list.has_next, Some(false));
        assert!(!list.is_empty());
        assert!(PostList::default().is_empty());
    }

    #[test]
    fn every_derive_is_exercised() {
        let p = post("p1", "");
        let l = PostList::default();
        assert_eq!(p, p.clone());
        assert_eq!(l, l.clone());
        assert!(!format!("{p:?}").is_empty());
        assert!(!format!("{l:?}").is_empty());
        let round: Post = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(round, p);
        let round: PostList = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(round, l);
    }
}
