//! Posts and the paged list the server returns them in.

use crate::{ChannelId, FileId, MessageAttachment, PostId, Reaction, Timestamp, UserId};
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

/// One file attached to a post.
///
/// Read out of `metadata.files` rather than modelled as a field, because the
/// same post arrives with and without its metadata depending on the endpoint,
/// and a missing `metadata` must mean "not loaded" rather than "no files".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileRef {
    pub id: FileId,
    pub name: String,
    pub extension: String,
    pub size: i64,
    pub mime_type: String,
    /// Zero for anything that is not an image.
    pub width: i64,
    pub height: i64,
    /// Whether the server generated a downscaled copy. When it did,
    /// `/preview` is what to fetch; when it did not, the file is already
    /// small enough to show as it is.
    pub has_preview_image: bool,
    /// A base64 JPEG thumbnail, a couple of kilobytes, already in the
    /// metadata. It costs no request at all, so a row can show *something*
    /// at the right size the instant it is drawn and never reflow when the
    /// real image lands.
    pub mini_preview: String,
}

impl FileRef {
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/") || (self.width > 0 && self.height > 0)
    }

    /// How big to draw this, fitted inside `max`, keeping its proportions.
    ///
    /// Returns `None` when the server did not state the dimensions, which is
    /// the case for anything that is not an image and occasionally for one
    /// that is: nothing can be reserved for a size nobody knows.
    #[must_use]
    pub fn fitted(&self, max_width: i64, max_height: i64) -> Option<(i32, i32)> {
        if self.width <= 0 || self.height <= 0 {
            return None;
        }
        // Scale by whichever dimension is further over, and never enlarge a
        // small image to fill the box -- a 16-pixel icon blown up to 400 is
        // worse than a 16-pixel icon.
        let by_width = f64::from(i32::try_from(max_width).unwrap_or(i32::MAX))
            / f64::from(i32::try_from(self.width).unwrap_or(i32::MAX));
        let by_height = f64::from(i32::try_from(max_height).unwrap_or(i32::MAX))
            / f64::from(i32::try_from(self.height).unwrap_or(i32::MAX));
        let scale = by_width.min(by_height).min(1.0);
        let scaled = |n: i64| -> i32 {
            let n = f64::from(i32::try_from(n).unwrap_or(i32::MAX)) * scale;
            // At least one pixel: a row with a zero-height image reads as a
            // missing image rather than a small one.
            #[allow(clippy::cast_possible_truncation)]
            let rounded = n.round() as i32;
            rounded.max(1)
        };
        Some((scaled(self.width), scaled(self.height)))
    }

    /// A size a person can read.
    #[must_use]
    pub fn human_size(&self) -> String {
        const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
        if self.size <= 0 {
            return String::new();
        }
        #[allow(clippy::cast_precision_loss)]
        let mut value = self.size as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{} {}", self.size, UNITS[0])
        } else {
            format!("{value:.1} {}", UNITS[unit])
        }
    }
}

impl Post {
    /// The files attached to this post.
    ///
    /// A post whose only content is an attachment has an empty `message`, so a
    /// client that ignores this renders an author, a timestamp and nothing —
    /// which reads as the client being broken rather than as a file.
    #[must_use]
    pub fn attachments(&self) -> Vec<FileRef> {
        let from_metadata = self
            .metadata
            .get("files")
            .and_then(|files| serde_json::from_value::<Vec<FileRef>>(files.clone()).ok());
        if let Some(files) = from_metadata
            && !files.is_empty()
        {
            return files;
        }
        // Metadata was not included by this endpoint. The ids are still known,
        // which is enough to say "there is a file here" rather than nothing.
        self.file_ids
            .iter()
            .map(|id| FileRef {
                id: id.clone(),
                ..FileRef::default()
            })
            .collect()
    }

    /// The Slack-compatible attachments in `props`.
    ///
    /// A webhook post carries its whole content here and leaves `message`
    /// empty, so this is not an extra: for those posts it is the message.
    #[must_use]
    pub fn message_attachments(&self) -> Vec<MessageAttachment> {
        let Some(raw) = self.props.get("attachments") else {
            return Vec::new();
        };
        serde_json::from_value::<Vec<MessageAttachment>>(raw.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !a.is_empty())
            .collect()
    }

    /// The name a webhook asked to post under, which is not its account's.
    ///
    /// An alerting integration posts as one bot account under many names --
    /// "Alertmanager", "Deploys", "CI" -- and showing the account instead
    /// labels every alert in the organisation with the same wrong word.
    #[must_use]
    pub fn override_username(&self) -> Option<&str> {
        self.props
            .get("override_username")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// Whether this came from an incoming webhook rather than a person.
    ///
    /// The flag is the **string** `"true"`, like most of this API's
    /// booleans; read as a JSON boolean it is always false.
    #[must_use]
    pub fn is_from_webhook(&self) -> bool {
        self.props
            .get("from_webhook")
            .and_then(serde_json::Value::as_str)
            == Some("true")
    }

    /// The link previews the server resolved for URLs in this message.
    ///
    /// Read out of `metadata.embeds` by hand rather than derived, because
    /// an embed is a tagged union whose `data` is `null` for every type
    /// but this one -- and `null` for this one too until the server has
    /// fetched the page, or forever if link previews are switched off.
    /// A preview with no title and no description is not drawn: a card
    /// showing only a URL says less than the link already in the text.
    #[must_use]
    pub fn link_previews(&self) -> Vec<LinkPreview> {
        let Some(embeds) = self
            .metadata
            .get("embeds")
            .and_then(serde_json::Value::as_array)
        else {
            return Vec::new();
        };
        embeds
            .iter()
            .filter(|embed| {
                embed.get("type").and_then(serde_json::Value::as_str) == Some("opengraph")
            })
            .filter_map(|embed| {
                let data = embed.get("data")?;
                let text = |key: &str| {
                    data.get(key)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let preview = LinkPreview {
                    url: embed
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    title: text("title"),
                    description: text("description"),
                    site_name: text("site_name"),
                    image: first_image(data),
                };
                (!preview.title.is_empty() || !preview.description.is_empty()).then_some(preview)
            })
            .collect()
    }

    /// The reactions the server attached to this post, or `None` when the
    /// endpoint did not include metadata at all.
    ///
    /// The distinction matters: `Some(vec![])` means "nobody has reacted",
    /// and `None` means "not loaded". Collapsing them would make a page
    /// fetched without metadata silently wipe reactions already held.
    #[must_use]
    pub fn reactions(&self) -> Option<Vec<Reaction>> {
        let raw = self.metadata.get("reactions")?;
        if raw.is_null() {
            // The server sends `"reactions": null` for a post nobody has
            // reacted to, which is a statement, not an absence.
            return Some(Vec::new());
        }
        serde_json::from_value(raw.clone()).ok()
    }

    #[must_use]
    pub fn has_attachments(&self) -> bool {
        !self.file_ids.is_empty()
            || self
                .metadata
                .get("files")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|files| !files.is_empty())
    }

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

    /// A Google Meet invitation renders as a card, which is the whole
    /// point of `metadata.embeds`: the server has already fetched the
    /// page, so the client draws what it found instead of showing a bare
    /// URL. The other embed types carry `data: null` and must not
    /// produce an empty card.
    #[test]
    fn link_previews_are_read_out_of_the_embeds_metadata() {
        let post = Post {
            metadata: serde_json::json!({
                "embeds": [
                    { "type": "link", "url": "https://jira.example/PP-1", "data": null },
                    {
                        "type": "opengraph",
                        "url": "https://meet.google.com/oty-uizw-zhg",
                        "data": {
                            "title": "Meet",
                            "description": "Real-time meetings by Google.",
                            "site_name": "Google",
                            "images": [
                                { "url": "", "secure_url": "https://ssl.gstatic.com/meet.png" }
                            ]
                        }
                    },
                    {
                        "type": "opengraph",
                        "url": "https://nothing.example/",
                        "data": { "title": "", "description": "" }
                    }
                ]
            }),
            ..Post::default()
        };
        assert_eq!(
            post.link_previews(),
            [LinkPreview {
                url: "https://meet.google.com/oty-uizw-zhg".into(),
                title: "Meet".into(),
                description: "Real-time meetings by Google.".into(),
                site_name: "Google".into(),
                image: Some("https://ssl.gstatic.com/meet.png".into()),
            }]
        );

        // No metadata at all, and metadata with no embeds, are both
        // simply no previews rather than a panic.
        assert!(Post::default().link_previews().is_empty());
        assert!(
            Post {
                metadata: serde_json::json!({ "embeds": null }),
                ..Post::default()
            }
            .link_previews()
            .is_empty()
        );
        // An image list with nothing usable in it still gives a card.
        let bare = Post {
            metadata: serde_json::json!({
                "embeds": [{
                    "type": "opengraph",
                    "url": "https://x.example/",
                    "data": { "title": "X", "images": [] }
                }]
            }),
            ..Post::default()
        };
        assert_eq!(bare.link_previews()[0].image, None);
    }

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

    /// A post whose only content is a file has an empty message. Ignoring
    /// metadata.files is what renders those as an author, a timestamp and
    /// nothing at all.
    #[test]
    fn attachments_come_from_metadata() {
        let p: Post = serde_json::from_str(
            r#"{"id":"p1","message":"",
                "metadata":{"files":[{"id":"f1","name":"photo.jpg","extension":"jpg",
                                      "size":2048,"mime_type":"image/jpeg",
                                      "width":800,"height":600}]}}"#,
        )
        .unwrap();
        assert!(p.has_attachments());
        let files = p.attachments();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "photo.jpg");
        assert!(files[0].is_image());
        assert_eq!(files[0].human_size(), "2.0 KB");
    }

    /// Some endpoints omit metadata entirely. The ids alone are still enough
    /// to say "there is a file here", which beats showing nothing.
    #[test]
    fn attachments_fall_back_to_file_ids_when_metadata_is_absent() {
        let p: Post =
            serde_json::from_str(r#"{"id":"p1","message":"","file_ids":["f1","f2"]}"#).unwrap();
        assert!(p.has_attachments());
        let files = p.attachments();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].id, FileId::new("f1"));
        assert!(files[0].name.is_empty());
    }

    #[test]
    fn a_post_with_no_files_has_no_attachments() {
        let p = post("p1", "");
        assert!(!p.has_attachments());
        assert!(p.attachments().is_empty());
    }

    #[test]
    fn a_non_image_file_is_not_an_image() {
        let doc = FileRef {
            mime_type: "application/pdf".into(),
            size: 1_500_000,
            ..FileRef::default()
        };
        assert!(!doc.is_image());
        assert_eq!(doc.human_size(), "1.4 MB");
    }

    #[test]
    fn sizes_render_at_every_scale_and_vanish_when_unknown() {
        let sized = |size| {
            FileRef {
                size,
                ..FileRef::default()
            }
            .human_size()
        };
        assert_eq!(sized(0), "", "an unknown size shows nothing, not '0 B'");
        assert_eq!(sized(-1), "");
        assert_eq!(sized(512), "512 B");
        assert_eq!(sized(1024), "1.0 KB");
        assert_eq!(sized(1_048_576), "1.0 MB");
        assert_eq!(sized(1_073_741_824), "1.0 GB");
        // Beyond the largest unit it keeps counting in GB rather than panicking.
        assert!(sized(1_099_511_627_776).ends_with("GB"));
    }

    #[test]
    fn every_derive_is_exercised() {
        let f = FileRef::default();
        assert_eq!(f, f.clone());
        assert!(!format!("{f:?}").is_empty());
        let round: FileRef = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(round, f);

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

#[cfg(test)]
mod reaction_tests {
    use super::*;

    fn post(metadata: &str) -> Post {
        serde_json::from_str(&format!(r#"{{"id":"p1","metadata":{metadata}}}"#)).expect("parses")
    }

    /// Reactions ride in metadata, like files do.
    #[test]
    fn reactions_come_from_metadata() {
        let p = post(r#"{"reactions":[{"user_id":"u1","emoji_name":"tada"}]}"#);
        let reactions = p.reactions().expect("metadata was present");
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].emoji_name, "tada");
    }

    /// "Nobody reacted" and "this endpoint did not send metadata" are
    /// different answers. Collapsing them lets a page fetched without
    /// metadata wipe reactions that are already held.
    #[test]
    fn an_absent_metadata_is_not_an_empty_reaction_list() {
        assert_eq!(post("{}").reactions(), None);
        assert_eq!(post(r#"{"reactions":null}"#).reactions(), Some(Vec::new()));
        assert_eq!(post(r#"{"reactions":[]}"#).reactions(), Some(Vec::new()));
    }
}

#[cfg(test)]
mod file_size_tests {
    use super::*;

    fn image(width: i64, height: i64) -> FileRef {
        FileRef {
            width,
            height,
            mime_type: "image/png".into(),
            ..FileRef::default()
        }
    }

    /// A big image is scaled down by whichever side is further over, keeping
    /// its proportions -- a squashed screenshot is worse than a small one.
    #[test]
    fn a_large_image_is_fitted_inside_the_box() {
        assert_eq!(image(1920, 1080).fitted(400, 300), Some((400, 225)));
        // Tall rather than wide: the height is what binds.
        assert_eq!(image(1080, 1920).fitted(400, 300), Some((169, 300)));
    }

    /// A small image is left alone. Blowing a 16-pixel icon up to fill the
    /// box makes it worse, not bigger.
    #[test]
    fn a_small_image_is_not_enlarged() {
        assert_eq!(image(16, 16).fitted(400, 300), Some((16, 16)));
    }

    /// An image whose dimensions the server did not state cannot have space
    /// reserved for it, and saying so is better than guessing a size and
    /// reflowing the list when the real one arrives.
    #[test]
    fn an_unmeasured_file_has_no_size() {
        assert_eq!(image(0, 0).fitted(400, 300), None);
        let document = FileRef {
            mime_type: "application/pdf".into(),
            ..FileRef::default()
        };
        assert_eq!(document.fitted(400, 300), None);
    }

    /// Rounding must never produce a zero dimension: a row with a
    /// zero-height image reads as a missing image rather than a thin one.
    #[test]
    fn an_extreme_aspect_ratio_still_has_a_pixel() {
        let (w, h) = image(4000, 1).fitted(400, 300).expect("an image");
        assert_eq!(w, 400);
        assert_eq!(h, 1);
    }

    /// The metadata the preview path depends on round-trips from the live
    /// payload shape.
    #[test]
    fn the_preview_metadata_parses() {
        let post: Post = serde_json::from_str(
            r#"{"id":"p1","metadata":{"files":[{"id":"f1","name":"lab.png",
                "extension":"png","size":4164,"mime_type":"image/png",
                "width":240,"height":120,"has_preview_image":true,
                "mini_preview":"/9j/2wCEAA=="}]}}"#,
        )
        .expect("parses");
        let file = &post.attachments()[0];
        assert!(file.is_image());
        assert!(file.has_preview_image);
        assert_eq!(file.mini_preview, "/9j/2wCEAA==");
        assert_eq!(file.fitted(400, 300), Some((240, 120)));
    }
}

#[cfg(test)]
mod webhook_tests {
    use super::*;

    /// Captured from a real alerting webhook on 12.0.1, with the alert text
    /// replaced. The shape is the point: `message` is empty, `type` is
    /// `slack_attachment`, and everything is in `props`.
    const ALERT: &str = r#"{
      "id":"p1","message":"","type":"slack_attachment",
      "props":{
        "from_webhook":"true",
        "override_username":"Alertmanager",
        "override_icon_url":"https://example.invalid/icon.png",
        "webhook_display_name":"techalerts",
        "attachments":[{"id":0,"color":"warning","fallback":"a summary",
          "title":"[FIRING:1] SomethingBroke","title_link":"https://example.invalid/a",
          "text":"Severity: Warning"}]
      }}"#;

    /// The bug this exists for: a post whose whole content is in `props`
    /// drew as an author and a timestamp with nothing under them.
    #[test]
    fn a_webhook_post_carries_its_content_in_attachments() {
        let post: Post = serde_json::from_str(ALERT).expect("parses");
        assert!(post.message.is_empty(), "the message really is empty");

        let attachments = post.message_attachments();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].title, "[FIRING:1] SomethingBroke");
        assert_eq!(attachments[0].text, "Severity: Warning");
        assert_eq!(attachments[0].rgb(), Some((0xff, 0xbc, 0x1f)));
    }

    /// One bot account posts under many names, so the account's username is
    /// the wrong label for every one of them.
    #[test]
    fn the_name_a_webhook_asked_for_wins_over_its_account() {
        let post: Post = serde_json::from_str(ALERT).expect("parses");
        assert_eq!(post.override_username(), Some("Alertmanager"));
        assert!(post.is_from_webhook());
    }

    /// `from_webhook` is the string `"true"`, like most of this API's
    /// booleans. Read as a JSON boolean it is always false.
    #[test]
    fn the_webhook_flag_is_a_string_not_a_boolean() {
        let as_boolean: Post =
            serde_json::from_str(r#"{"id":"p1","props":{"from_webhook":true}}"#).expect("parses");
        assert!(
            !as_boolean.is_from_webhook(),
            "a real boolean is not what the server sends"
        );
        let as_string: Post =
            serde_json::from_str(r#"{"id":"p1","props":{"from_webhook":"true"}}"#).expect("parses");
        assert!(as_string.is_from_webhook());
    }

    /// An ordinary message has neither, and must not grow a BOT tag.
    #[test]
    fn an_ordinary_post_has_no_override_and_no_attachments() {
        let post: Post = serde_json::from_str(r#"{"id":"p1","message":"hello"}"#).expect("parses");
        assert_eq!(post.override_username(), None);
        assert!(!post.is_from_webhook());
        assert!(post.message_attachments().is_empty());
    }

    /// Malformed or empty attachments must not take the message down with
    /// them: whatever else is on the post still has to render.
    #[test]
    fn unusable_attachments_are_dropped_rather_than_failing() {
        let nonsense: Post =
            serde_json::from_str(r#"{"id":"p1","props":{"attachments":"not a list"}}"#)
                .expect("parses");
        assert!(nonsense.message_attachments().is_empty());

        // An attachment with only a colour would draw as a bare coloured bar.
        let hollow: Post =
            serde_json::from_str(r#"{"id":"p1","props":{"attachments":[{"color":"good"}]}}"#)
                .expect("parses");
        assert!(hollow.message_attachments().is_empty());

        // A blank override is not a name.
        let blank: Post = serde_json::from_str(r#"{"id":"p1","props":{"override_username":"  "}}"#)
            .expect("parses");
        assert_eq!(blank.override_username(), None);
    }
}

/// What the server found behind a link in a message.
///
/// Not a serde type: the server's own shape is a union with a `data`
/// that is `null` for most of its variants, so it is read field by field
/// out of the opaque metadata instead. See `Post::link_previews`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub site_name: String,
    /// The first image the page offered, if any. `secure_url` is
    /// preferred over `url`: the plain one is often `http://` even when
    /// the page is served over TLS.
    pub image: Option<String>,
}

/// The first usable image URL in an `OpenGraph` payload.
fn first_image(data: &serde_json::Value) -> Option<String> {
    let images = data.get("images")?.as_array()?;
    images.iter().find_map(|image| {
        ["secure_url", "url"]
            .iter()
            .filter_map(|key| image.get(*key).and_then(serde_json::Value::as_str))
            .find(|url| !url.is_empty())
            .map(ToOwned::to_owned)
    })
}
