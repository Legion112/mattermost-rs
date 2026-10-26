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
#[path = "tests/unread.rs"]
mod tests;
