//! Unread and mention computation.
//!
//! The arithmetic is not guesswork: it is the server's own, taken from
//! `GetChannelUnread` in `server/channels/store/sqlstore/channel_store.go`,
//! which selects `Channels.TotalMsgCount - ChannelMembers.MsgCount` and the
//! `*Root` variants, and reads the mention counts straight off the member row.
//!
//! Three facts about this are easy to get wrong and expensive to get wrong,
//! so they are encoded in the types rather than left to call sites:
//!
//! 1. **Mention counts are stored, never derived.** The server maintains them;
//!    a client that recomputes them from message text will disagree with the
//!    badge on every other device.
//! 2. **`notify_props.mark_unread == "mention"` *is* the muted state.** There
//!    is no separate muted flag on the wire. Muting suppresses the unread
//!    marker only -- a muted channel still shows a mention badge.
//! 3. **With collapsed reply threads on, replies do not make a channel
//!    unread.** That is the entire reason the `*_root` fields exist. Getting
//!    this backwards is the most visible possible bug in the client.

/// The channel-side half of the computation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelTotals {
    /// Every message in the channel, replies included.
    pub total_msg_count: i64,
    /// Root posts only.
    pub total_msg_count_root: i64,
}

/// My own membership's half. Deliberately *my* membership and not the channel
/// roster: unread never needs anybody else's counts, and the two arrive from
/// different endpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemberCounts {
    /// How many messages I have seen, replies included.
    pub msg_count: i64,
    /// How many root posts I have seen.
    pub msg_count_root: i64,
    /// Mentions awaiting me, replies included. Server-maintained.
    pub mention_count: i64,
    /// Mentions awaiting me in root posts. Server-maintained.
    pub mention_count_root: i64,
}

/// Whether collapsed reply threads are in effect, which decides whether the
/// channel's unread state counts replies.
///
/// This is a two-level decision upstream -- the server's
/// `ServiceSettings.CollapsedThreads` combined with the user's
/// `display.collapsed_reply_threads` preference -- and it must never be
/// hardcoded: the server default was `disabled` historically and is
/// `always_on` as of 12.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threading {
    /// Replies count toward the channel's unread state.
    Flat,
    /// Only root posts count; reply unreads surface per-thread instead.
    Collapsed,
}

/// The `mark_unread` notify prop, which is how muting is represented.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarkUnread {
    /// Any message marks the channel unread. The default.
    #[default]
    All,
    /// Only mentions do -- i.e. the channel is muted.
    MentionsOnly,
}

impl MarkUnread {
    /// Parse a raw `notify_props.mark_unread` value.
    ///
    /// Anything other than `"mention"` means "all", including the literal
    /// `"default"` the server writes for an untouched channel and including
    /// values a future server version might add: treating an unknown value as
    /// "not muted" errs toward showing the user their messages.
    #[must_use]
    pub fn from_notify_prop(raw: &str) -> Self {
        match raw {
            "mention" => Self::MentionsOnly,
            _ => Self::All,
        }
    }
}

/// What a channel owes the reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Unread {
    /// Unread messages, already clamped at zero.
    pub messages: i64,
    /// Mentions awaiting the reader.
    pub mentions: i64,
}

impl Unread {
    /// Whether the channel should read as unread in the sidebar.
    ///
    /// A muted channel is unread only when it holds a mention; that is the
    /// whole meaning of `mark_unread: "mention"`.
    #[must_use]
    pub fn is_unread(self, mark_unread: MarkUnread) -> bool {
        match mark_unread {
            MarkUnread::All => self.messages > 0 || self.mentions > 0,
            MarkUnread::MentionsOnly => self.mentions > 0,
        }
    }

    /// The number to draw in the badge, which is mentions and never messages.
    ///
    /// Muting does not suppress this. A muted channel with a direct mention
    /// still shows the count, because the alternative is silently losing a
    /// message addressed to the reader by name.
    #[must_use]
    pub fn badge(self) -> i64 {
        self.mentions
    }
}

/// Compute a channel's unread state.
///
/// The clamp at zero is load-bearing, not defensive tidiness: the server's
/// `UpdateLastViewedAt` assigns `greatest(cm.MsgCount, c.TotalMsgCount)`, so a
/// member counter can legitimately sit *above* the channel total for as long as
/// it takes the next channel update to arrive. Without the clamp that window
/// renders as a negative unread count.
#[must_use]
pub fn unread(totals: ChannelTotals, member: MemberCounts, threading: Threading) -> Unread {
    let (total, seen, mentions) = match threading {
        Threading::Flat => (
            totals.total_msg_count,
            member.msg_count,
            member.mention_count,
        ),
        Threading::Collapsed => (
            totals.total_msg_count_root,
            member.msg_count_root,
            member.mention_count_root,
        ),
    };
    Unread {
        messages: (total - seen).max(0),
        mentions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture the Docker lab also seeds: 20 messages in the channel of
    /// which 4 are replies, the reader has seen 13 (11 of them root), and one
    /// mention is waiting in a root post.
    fn fixture() -> (ChannelTotals, MemberCounts) {
        (
            ChannelTotals {
                total_msg_count: 20,
                total_msg_count_root: 16,
            },
            MemberCounts {
                msg_count: 13,
                msg_count_root: 11,
                mention_count: 1,
                mention_count_root: 1,
            },
        )
    }

    #[test]
    fn flat_counts_replies() {
        let (totals, member) = fixture();
        assert_eq!(
            unread(totals, member, Threading::Flat),
            Unread {
                messages: 7,
                mentions: 1
            }
        );
    }

    /// With CRT on, replies must not make the channel unread -- the root
    /// delta is 16 - 11 = 5, not the flat 7.
    #[test]
    fn collapsed_counts_root_posts_only() {
        let (totals, member) = fixture();
        assert_eq!(
            unread(totals, member, Threading::Collapsed),
            Unread {
                messages: 5,
                mentions: 1
            }
        );
    }

    /// The server assigns `greatest(cm.MsgCount, c.TotalMsgCount)` when
    /// marking a channel read, so a member counter above the channel total is
    /// a normal transient state and must not render as a negative count.
    #[test]
    fn member_ahead_of_channel_clamps_to_zero() {
        let totals = ChannelTotals {
            total_msg_count: 5,
            total_msg_count_root: 5,
        };
        let member = MemberCounts {
            msg_count: 9,
            msg_count_root: 9,
            ..MemberCounts::default()
        };
        assert_eq!(unread(totals, member, Threading::Flat).messages, 0);
        assert_eq!(unread(totals, member, Threading::Collapsed).messages, 0);
    }

    #[test]
    fn unmuted_channel_is_unread_on_messages_alone() {
        let u = Unread {
            messages: 3,
            mentions: 0,
        };
        assert!(u.is_unread(MarkUnread::All));
    }

    /// A mention with no unread message delta still counts: the delta can be
    /// zero while a mention is outstanding, because the two are maintained
    /// independently by the server.
    #[test]
    fn unmuted_channel_is_unread_on_mentions_alone() {
        let u = Unread {
            messages: 0,
            mentions: 2,
        };
        assert!(u.is_unread(MarkUnread::All));
    }

    #[test]
    fn read_channel_is_not_unread() {
        assert!(!Unread::default().is_unread(MarkUnread::All));
        assert!(!Unread::default().is_unread(MarkUnread::MentionsOnly));
    }

    /// Muting suppresses the unread marker for ordinary messages. This is the
    /// entire behavioural content of `mark_unread: "mention"`.
    #[test]
    fn muted_channel_ignores_plain_messages() {
        let u = Unread {
            messages: 40,
            mentions: 0,
        };
        assert!(!u.is_unread(MarkUnread::MentionsOnly));
    }

    /// ...but a mention still breaks through, and still draws a badge. A
    /// client that suppressed this would silently drop a message addressed to
    /// the reader by name.
    #[test]
    fn muted_channel_still_surfaces_mentions() {
        let u = Unread {
            messages: 40,
            mentions: 1,
        };
        assert!(u.is_unread(MarkUnread::MentionsOnly));
        assert_eq!(u.badge(), 1);
    }

    #[test]
    fn badge_is_mentions_never_messages() {
        let u = Unread {
            messages: 99,
            mentions: 3,
        };
        assert_eq!(u.badge(), 3);
    }

    #[test]
    fn mark_unread_parses_mention_as_muted() {
        assert_eq!(
            MarkUnread::from_notify_prop("mention"),
            MarkUnread::MentionsOnly
        );
    }

    /// "default" is what the server writes for an untouched channel, and an
    /// unknown value from a future server must not read as muted -- erring
    /// toward showing the reader their messages.
    #[test]
    fn mark_unread_parses_everything_else_as_all() {
        for raw in ["all", "default", "", "something-new"] {
            assert_eq!(MarkUnread::from_notify_prop(raw), MarkUnread::All, "{raw}");
        }
        assert_eq!(MarkUnread::default(), MarkUnread::All);
    }

    /// Not a behaviour test. `#[derive]` emits code attributed to the derive
    /// line, so an unexercised Debug or Clone impl reads as an uncovered line
    /// and fails the 100% gate on a line that contains no logic. Exercising
    /// every derive on every public type once keeps the gate honest instead of
    /// making it noisy -- see the plan's note on this pattern.
    #[test]
    fn every_derive_is_exercised() {
        let totals = ChannelTotals::default();
        let member = MemberCounts::default();
        let un = Unread::default();
        let mark = MarkUnread::default();
        let threading = Threading::Flat;

        assert_eq!(totals, totals.clone());
        assert_eq!(member, member.clone());
        assert_eq!(un, un.clone());
        assert_eq!(mark, mark.clone());
        assert_eq!(threading, threading.clone());
        assert_ne!(threading, Threading::Collapsed);

        for rendered in [
            format!("{totals:?}"),
            format!("{member:?}"),
            format!("{un:?}"),
            format!("{mark:?}"),
            format!("{threading:?}"),
        ] {
            assert!(!rendered.is_empty());
        }
    }
}
