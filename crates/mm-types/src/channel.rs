//! Channels and my membership of them.

use crate::{ChannelId, TeamId, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The single-letter channel type the server uses.
///
/// Represented so that an unknown letter round-trips instead of failing. The
/// server has added types over time (`S` for shared channels, and bot variants
/// have appeared in payloads), and a client that refused to deserialize a
/// channel list because one entry had a new type would be unusable on an
/// upgraded server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ChannelType {
    /// `O` — a public channel.
    #[default]
    Open,
    /// `P` — a private channel.
    Private,
    /// `D` — a one-to-one direct message.
    Direct,
    /// `G` — a group direct message.
    Group,
    /// Anything the server sent that we do not model.
    Other(String),
}

impl ChannelType {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Open => "O",
            Self::Private => "P",
            Self::Direct => "D",
            Self::Group => "G",
            Self::Other(raw) => raw,
        }
    }

    /// A DM or group DM, which the sidebar groups separately from channels and
    /// which has no meaningful team.
    #[must_use]
    pub fn is_dm(&self) -> bool {
        matches!(self, Self::Direct | Self::Group)
    }
}

impl From<String> for ChannelType {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "O" => Self::Open,
            "P" => Self::Private,
            "D" => Self::Direct,
            "G" => Self::Group,
            _ => Self::Other(raw),
        }
    }
}

impl From<ChannelType> for String {
    fn from(t: ChannelType) -> Self {
        t.as_str().to_owned()
    }
}

/// A channel.
///
/// `total_msg_count` and `total_msg_count_root` are the channel-side half of
/// the unread computation; the other half is on [`ChannelMember`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: ChannelId,
    pub create_at: Timestamp,
    pub update_at: Timestamp,
    /// Zero means "not deleted"; the server soft-deletes.
    pub delete_at: Timestamp,
    /// Empty for DMs and group DMs.
    pub team_id: TeamId,
    #[serde(rename = "type")]
    pub kind: ChannelType,
    /// The URL-safe name. For a DM this is the two user ids joined by `__`,
    /// which is why DM display names have to be derived from the roster.
    ///
    /// Every string here tolerates `null`, like the ones on `User`:
    /// `#[serde(default)]` covers a missing key, not a present one
    /// holding null, and one null in one channel would otherwise empty
    /// the whole sidebar.
    #[serde(deserialize_with = "crate::wire::null_default")]
    pub name: String,
    #[serde(deserialize_with = "crate::wire::null_default")]
    pub display_name: String,
    #[serde(deserialize_with = "crate::wire::null_default")]
    pub header: String,
    #[serde(deserialize_with = "crate::wire::null_default")]
    pub purpose: String,
    pub last_post_at: Timestamp,
    pub last_root_post_at: Timestamp,
    pub total_msg_count: i64,
    pub total_msg_count_root: i64,
}

impl Channel {
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        !self.delete_at.is_zero()
    }
}

/// `notify_props.mark_unread`. There is no separate "muted" flag on the wire:
/// muting a channel *is* setting this to `mention`.
pub const MARK_UNREAD: &str = "mark_unread";
/// The value of [`MARK_UNREAD`] that means muted.
pub const MARK_UNREAD_MENTION: &str = "mention";

/// My membership of a channel — the unread source of truth.
///
/// Note the two `Option` fields. The server does not zero `last_viewed_at` and
/// `last_update_at` when returning *another* user's membership; it omits them
/// entirely (it writes a `-1` sentinel and drops them with `omitempty`). So
/// `None` means "this is somebody else's membership", which is a different
/// thing from "never viewed".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelMember {
    pub channel_id: ChannelId,
    pub user_id: UserId,
    pub roles: String,
    pub last_viewed_at: Option<Timestamp>,
    pub last_update_at: Option<Timestamp>,
    pub msg_count: i64,
    pub msg_count_root: i64,
    pub mention_count: i64,
    pub mention_count_root: i64,
    pub urgent_mention_count: i64,
    pub notify_props: BTreeMap<String, String>,
}

impl ChannelMember {
    /// Whether the channel is muted.
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.notify_props.get(MARK_UNREAD).map(String::as_str) == Some(MARK_UNREAD_MENTION)
    }

    /// Whether this membership is mine, in the sense that the server included
    /// the fields it only discloses to the owner.
    #[must_use]
    pub fn is_own(&self) -> bool {
        self.last_viewed_at.is_some()
    }
}

/// `GET /channels/{id}/stats`.
///
/// The member count the channel header shows. Note `pinnedpost_count`: the
/// server spells it without the second underscore, unlike every neighbouring
/// field, and a `#[serde(default)]` struct would silently read zero forever.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelStats {
    pub channel_id: ChannelId,
    pub member_count: i64,
    pub guest_count: i64,
    #[serde(rename = "pinnedpost_count")]
    pub pinned_post_count: i64,
    pub files_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string on a channel tolerates `null`, for the same reason
    /// the ones on `User` do: `#[serde(default)]` covers a missing key,
    /// not a present one holding null, and one null in one channel
    /// would empty the entire sidebar rather than spoil one row.
    #[test]
    fn a_null_string_on_a_channel_is_not_a_parse_failure() {
        let channel: Channel = serde_json::from_str(
            r#"{"id":"c1","type":"O","name":null,"display_name":null,
                "header":null,"purpose":null}"#,
        )
        .unwrap();
        assert_eq!(channel.id, ChannelId::new("c1"));
        assert!(channel.name.is_empty());
        assert!(channel.display_name.is_empty());
        assert!(channel.header.is_empty());
        assert!(channel.purpose.is_empty());
    }

    #[test]
    fn known_channel_types_round_trip_through_their_letters() {
        for (letter, kind) in [
            ("O", ChannelType::Open),
            ("P", ChannelType::Private),
            ("D", ChannelType::Direct),
            ("G", ChannelType::Group),
        ] {
            let parsed: ChannelType = serde_json::from_str(&format!("\"{letter}\"")).unwrap();
            assert_eq!(parsed, kind, "{letter}");
            assert_eq!(kind.as_str(), letter);
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{letter}\"")
            );
        }
    }

    /// An unknown type must survive deserialization. The server has grown
    /// channel types, and refusing to parse a channel list because one entry
    /// is new would make the client unusable on an upgraded server.
    #[test]
    fn unknown_channel_type_round_trips_instead_of_failing() {
        let parsed: ChannelType = serde_json::from_str("\"S\"").unwrap();
        assert_eq!(parsed, ChannelType::Other("S".into()));
        assert_eq!(parsed.as_str(), "S");
        assert_eq!(serde_json::to_string(&parsed).unwrap(), "\"S\"");
    }

    #[test]
    fn dm_detection_covers_both_dm_shapes() {
        assert!(ChannelType::Direct.is_dm());
        assert!(ChannelType::Group.is_dm());
        assert!(!ChannelType::Open.is_dm());
        assert!(!ChannelType::Private.is_dm());
        assert!(!ChannelType::Other("S".into()).is_dm());
        assert_eq!(ChannelType::default(), ChannelType::Open);
    }

    /// Every field the client does not model must be ignored rather than
    /// rejected: the server adds fields constantly, and a strict client breaks
    /// on the next minor release.
    #[test]
    fn unknown_fields_are_ignored() {
        let channel: Channel = serde_json::from_str(
            r#"{"id":"c1","type":"O","display_name":"Town Square",
                "total_msg_count":20,"total_msg_count_root":16,
                "some_field_from_the_future":{"nested":true}}"#,
        )
        .unwrap();
        assert_eq!(channel.display_name, "Town Square");
        assert_eq!(channel.total_msg_count, 20);
        assert_eq!(channel.total_msg_count_root, 16);
    }

    /// Absent fields must default, because the server omits plenty of them
    /// depending on the endpoint.
    #[test]
    fn absent_fields_default() {
        let channel: Channel = serde_json::from_str(r#"{"id":"c1"}"#).unwrap();
        assert_eq!(channel.id, ChannelId::new("c1"));
        assert_eq!(channel.kind, ChannelType::Open);
        assert_eq!(channel.total_msg_count, 0);
        assert!(!channel.is_deleted());
    }

    #[test]
    fn soft_delete_is_a_nonzero_timestamp() {
        let mut channel = Channel::default();
        assert!(!channel.is_deleted());
        channel.delete_at = Timestamp(1);
        assert!(channel.is_deleted());
    }

    /// `mark_unread: "mention"` IS the muted state — there is no other flag.
    #[test]
    fn muted_is_mark_unread_mention_and_nothing_else() {
        let mut m = ChannelMember::default();
        assert!(!m.is_muted(), "a default membership is not muted");

        m.notify_props.insert(MARK_UNREAD.into(), "all".to_owned());
        assert!(!m.is_muted());

        m.notify_props
            .insert(MARK_UNREAD.into(), "default".to_owned());
        assert!(!m.is_muted());

        m.notify_props
            .insert(MARK_UNREAD.into(), MARK_UNREAD_MENTION.to_owned());
        assert!(m.is_muted());
    }

    /// The server omits `last_viewed_at` for another user's membership rather
    /// than zeroing it, so None and Some(0) mean different things.
    #[test]
    fn omitted_last_viewed_at_marks_a_foreign_membership() {
        let mine: ChannelMember =
            serde_json::from_str(r#"{"channel_id":"c","last_viewed_at":0}"#).unwrap();
        assert!(mine.is_own());
        assert_eq!(mine.last_viewed_at, Some(Timestamp::ZERO));

        let theirs: ChannelMember = serde_json::from_str(r#"{"channel_id":"c"}"#).unwrap();
        assert!(!theirs.is_own());
        assert_eq!(theirs.last_viewed_at, None);
    }

    #[test]
    fn every_derive_is_exercised() {
        let c = Channel::default();
        let m = ChannelMember::default();
        let t = ChannelType::default();
        assert_eq!(c, c.clone());
        assert_eq!(m, m.clone());
        assert_eq!(t, t.clone());
        for s in [format!("{c:?}"), format!("{m:?}"), format!("{t:?}")] {
            assert!(!s.is_empty());
        }
        let round: Channel = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(round, c);
        let round: ChannelMember =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(round, m);
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    /// Captured from 12.0.1. `pinnedpost_count` is the whole reason this test
    /// exists: every other field in the payload uses underscores between
    /// words, and reading it as `pinned_post_count` parses happily and
    /// returns zero forever.
    #[test]
    fn the_pinned_post_count_is_spelled_without_the_second_underscore() {
        let json = r#"{"channel_id":"c1","member_count":2,"guest_count":0,
                       "pinnedpost_count":7,"files_count":3}"#;
        let stats: ChannelStats = serde_json::from_str(json).expect("parses");
        assert_eq!(stats.member_count, 2);
        assert_eq!(stats.pinned_post_count, 7);
        assert_eq!(stats.files_count, 3);
        assert_eq!(stats.channel_id, ChannelId::new("c1"));

        // And it goes back out the way it came in.
        let text = serde_json::to_string(&stats).expect("serializes");
        assert!(text.contains("pinnedpost_count"), "{text}");
    }
}
