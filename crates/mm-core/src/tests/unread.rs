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
