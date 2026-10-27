use super::*;
use mm_types::{ChannelType, PostList, TeamId};
use std::collections::BTreeMap;

const ME: &str = "u-me";

fn store() -> Store {
    Store::new(UserId::new(ME), Threading::Flat)
}

fn channel(id: &str, total: i64, root: i64) -> Channel {
    Channel {
        id: ChannelId::new(id),
        display_name: format!("Channel {id}"),
        kind: ChannelType::Open,
        total_msg_count: total,
        total_msg_count_root: root,
        ..Channel::default()
    }
}

fn member(channel: &str, seen: i64, seen_root: i64, mentions: i64) -> ChannelMember {
    ChannelMember {
        channel_id: ChannelId::new(channel),
        user_id: UserId::new(ME),
        last_viewed_at: Some(Timestamp(100)),
        last_update_at: Some(Timestamp(100)),
        msg_count: seen,
        msg_count_root: seen_root,
        mention_count: mentions,
        mention_count_root: mentions,
        ..ChannelMember::default()
    }
}

fn post(id: &str, channel: &str, created: i64) -> Post {
    Post {
        id: PostId::new(id),
        channel_id: ChannelId::new(channel),
        user_id: UserId::new("u-other"),
        create_at: Timestamp(created),
        update_at: Timestamp(created),
        message: format!("message {id}"),
        ..Post::default()
    }
}

fn page(posts: Vec<Post>) -> PostList {
    let mut map = std::collections::HashMap::new();
    let mut order: Vec<PostId> = posts.iter().map(|p| p.id.clone()).collect();
    order.reverse();
    for p in posts {
        map.insert(p.id.clone(), p);
    }
    PostList {
        order,
        posts: map,
        ..PostList::default()
    }
}

fn seeded() -> Store {
    let mut s = store();
    s.apply_channels(vec![channel("c1", 20, 16), channel("c2", 5, 5)]);
    s.apply_members(vec![member("c1", 13, 11, 1), member("c2", 5, 5, 0)]);
    s
}

// ------------------------------------------------------------ unread

#[test]
fn unread_uses_the_channel_and_member_counters() {
    let s = seeded();
    assert_eq!(
        s.unread(&ChannelId::new("c1")),
        Unread {
            messages: 7,
            mentions: 1
        }
    );
    assert_eq!(s.unread(&ChannelId::new("c2")), Unread::default());
}

#[test]
fn collapsed_threading_uses_the_root_counters() {
    let mut s = seeded();
    s.threading = Threading::Collapsed;
    assert_eq!(
        s.unread(&ChannelId::new("c1")),
        Unread {
            messages: 5,
            mentions: 1
        }
    );
}

#[test]
fn an_unknown_channel_has_no_unread_rather_than_panicking() {
    let s = seeded();
    assert_eq!(s.unread(&ChannelId::new("nope")), Unread::default());
    assert!(!s.is_muted(&ChannelId::new("nope")));
}

/// Muting suppresses the unread marker, not the badge, so a muted channel
/// still contributes its mentions to the total.
#[test]
fn the_badge_total_includes_muted_channels() {
    let mut s = seeded();
    let mut muted = member("c2", 0, 0, 3);
    muted
        .notify_props
        .insert("mark_unread".into(), "mention".into());
    s.apply_member(muted);

    assert!(s.is_muted(&ChannelId::new("c2")));
    assert_eq!(s.total_mentions(), 4, "1 from c1 plus 3 from muted c2");
}

// ------------------------------------------------------------ monotonic joins

/// An older channel payload must not drag `total_msg_count` back, or a channel
/// with unread messages reads as read until the next update lands.
#[test]
fn an_older_channel_payload_cannot_lower_the_message_total() {
    let mut s = store();
    let mut newer = channel("c1", 20, 16);
    newer.update_at = Timestamp(500);
    s.apply_channel(newer);

    let mut older = channel("c1", 3, 3);
    older.update_at = Timestamp(100);
    s.apply_channel(older);

    let held = &s.channels[&ChannelId::new("c1")];
    assert_eq!(held.total_msg_count, 20);
    assert_eq!(held.total_msg_count_root, 16);
}

/// A read-state push generated before a mention we already know about must not
/// clear the badge.
#[test]
fn a_stale_member_push_cannot_clear_a_mention() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    assert_eq!(s.unread(&c1).mentions, 1);

    let mut stale = member("c1", 13, 11, 0);
    stale.last_update_at = Some(Timestamp(50)); // older than the held 100
    s.apply_member(stale);

    assert_eq!(s.unread(&c1).mentions, 1, "a stale push must not win");
}

#[test]
fn a_fresh_member_push_does_update_the_mention_count() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let mut fresh = member("c1", 13, 11, 4);
    fresh.last_update_at = Some(Timestamp(900));
    s.apply_member(fresh);
    assert_eq!(s.unread(&c1).mentions, 4);
}

/// Seen counters only move forward. A push with a lower count would re-unread
/// messages the user has already read.
#[test]
fn seen_counters_never_move_backwards() {
    let mut s = seeded();
    let mut regressive = member("c1", 2, 2, 0);
    regressive.last_update_at = Some(Timestamp(900));
    s.apply_member(regressive);

    let held = &s.my_member[&ChannelId::new("c1")];
    assert_eq!(held.msg_count, 13);
    assert_eq!(held.msg_count_root, 11);
}

#[test]
fn last_viewed_at_takes_the_later_of_the_two() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");

    let mut earlier = member("c1", 13, 11, 1);
    earlier.last_viewed_at = Some(Timestamp(10));
    earlier.last_update_at = Some(Timestamp(900));
    s.apply_member(earlier);
    assert_eq!(s.my_member[&c1].last_viewed_at, Some(Timestamp(100)));

    let mut later = member("c1", 13, 11, 1);
    later.last_viewed_at = Some(Timestamp(999));
    later.last_update_at = Some(Timestamp(1000));
    s.apply_member(later);
    assert_eq!(s.my_member[&c1].last_viewed_at, Some(Timestamp(999)));
}

// ------------------------------------------------------------ posts

#[test]
fn a_page_populates_the_timeline_oldest_first() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(
        &c1,
        &page(vec![
            post("p1", "c1", 10),
            post("p2", "c1", 20),
            post("p3", "c1", 30),
        ]),
        PageAnchor::Newest,
    );
    let ids: Vec<&str> = s.channel_posts(&c1).iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["p1", "p2", "p3"]);
}

/// An empty `prev_post_id` is the server saying there is nothing older.
#[test]
fn a_page_with_no_previous_cursor_marks_the_start_reached() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);
    assert!(s.timelines[&c1].older_complete());
}

/// A replayed `posted` after a reconnect must not duplicate the message.
#[test]
fn a_replayed_posted_event_is_idempotent() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let event = WsEvent::Posted {
        post: post("p9", "c1", 900),
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    };

    let first = s.apply_ws(&event);
    assert!(first.contains(&Change::Post(PostId::new("p9"))));
    let before = s.channels[&c1].total_msg_count;

    let second = s.apply_ws(&event);
    assert!(second.is_empty(), "a duplicate must change nothing");
    assert_eq!(
        s.channels[&c1].total_msg_count, before,
        "and must not double-count toward unread"
    );
    assert_eq!(s.channel_posts(&c1).len(), 1);
}

/// The channel's own counter has to move, or the unread arithmetic will not
/// see the new message until the next channel fetch.
#[test]
fn a_new_post_raises_the_channel_total_and_the_unread_count() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let before = s.unread(&c1).messages;
    s.apply_ws(&WsEvent::Posted {
        post: post("p9", "c1", 900),
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });
    assert_eq!(s.unread(&c1).messages, before + 1);
}

/// With collapsed threading a reply must not make the channel unread — that is
/// the entire reason the root counters exist.
#[test]
fn a_reply_does_not_raise_the_root_total() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let mut reply = post("p9", "c1", 900);
    reply.root_id = PostId::new("p1");

    let root_before = s.channels[&c1].total_msg_count_root;
    s.apply_ws(&WsEvent::Posted {
        post: reply,
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });
    assert_eq!(s.channels[&c1].total_msg_count_root, root_before);
    assert_eq!(s.channels[&c1].total_msg_count, 21);
}

#[test]
fn an_edit_updates_the_message_text() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);

    let mut edited = post("p1", "c1", 10);
    edited.message = "edited".into();
    edited.update_at = Timestamp(50);
    edited.edit_at = Timestamp(50);
    s.apply_ws(&WsEvent::PostEdited(edited));

    assert_eq!(s.posts[&PostId::new("p1")].message, "edited");
}

/// An edit that is older than what is held must not win, or two devices racing
/// produce a flapping message body.
#[test]
fn an_out_of_order_edit_cannot_regress_a_newer_one() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);

    let mut newer = post("p1", "c1", 10);
    newer.message = "second".into();
    newer.update_at = Timestamp(200);
    s.apply_ws(&WsEvent::PostEdited(newer));

    let mut older = post("p1", "c1", 10);
    older.message = "first".into();
    older.update_at = Timestamp(100);
    s.apply_ws(&WsEvent::PostEdited(older));

    assert_eq!(s.posts[&PostId::new("p1")].message, "second");
}

/// A delete is a tombstone, not a removal: a late `posted` for the same id
/// must not resurrect it, and a permalink should still resolve.
#[test]
fn a_delete_leaves_a_tombstone_that_cannot_be_resurrected() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);

    let changes = s.apply_ws(&WsEvent::PostDeleted(post("p1", "c1", 10)));
    assert!(changes.contains(&Change::PostRemoved(PostId::new("p1"))));
    assert!(
        s.channel_posts(&c1).is_empty(),
        "it must leave the timeline"
    );
    assert!(
        s.posts[&PostId::new("p1")].is_deleted(),
        "but remain as a tombstone"
    );

    s.apply_ws(&WsEvent::Posted {
        post: post("p1", "c1", 10),
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });
    assert!(
        s.channel_posts(&c1).is_empty(),
        "a replayed posted must not resurrect a deleted post"
    );
}

// ------------------------------------------------------------ read state

#[test]
fn marking_read_locally_clears_the_unread_and_the_mentions() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.mark_read_locally(&c1);
    assert_eq!(s.unread(&c1), Unread::default());
}

#[test]
fn multiple_channels_viewed_clears_the_named_channels() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let mut times = BTreeMap::new();
    times.insert("c1".to_owned(), 900_i64);
    let changes = s.apply_ws(&WsEvent::MultipleChannelsViewed {
        channel_times: times,
    });
    assert_eq!(changes, vec![Change::ReadState]);
    assert_eq!(s.unread(&c1), Unread::default());
    assert_eq!(s.my_member[&c1].last_viewed_at, Some(Timestamp(900)));
}

/// The legacy event carries no timestamp, so the channel's own `last_post_at`
/// stands in for it.
#[test]
fn the_legacy_channel_viewed_event_also_clears_unread() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_ws(&WsEvent::ChannelViewed {
        channel_id: c1.clone(),
    });
    assert_eq!(s.unread(&c1), Unread::default());
}

#[test]
fn viewing_an_unknown_channel_is_harmless() {
    let mut s = seeded();
    let mut times = BTreeMap::new();
    times.insert("nope".to_owned(), 1_i64);
    s.apply_ws(&WsEvent::MultipleChannelsViewed {
        channel_times: times,
    });
    assert_eq!(s.unread(&ChannelId::new("c1")).messages, 7);
}

// ------------------------------------------------------------ sidebar

/// `HashMap` iteration order must never reach the UI, so the sidebar is sorted
/// — unread first, then by label.
#[test]
fn the_sidebar_is_deterministic_with_unread_channels_first() {
    let mut s = store();
    s.apply_channels(vec![
        channel("a", 0, 0),
        channel("b", 9, 9),
        channel("c", 0, 0),
    ]);
    s.apply_members(vec![
        member("a", 0, 0, 0),
        member("b", 0, 0, 0),
        member("c", 0, 0, 0),
    ]);

    let first = s.sidebar();
    let second = s.sidebar();
    assert_eq!(first, second, "the order must be stable");

    let names: Vec<&str> = first.iter().map(|r| r.display_name.as_str()).collect();
    assert_eq!(names[0], "Channel b", "the unread channel comes first");
    assert_eq!(&names[1..], &["Channel a", "Channel c"]);
    assert!(s.any_unread());
}

#[test]
fn a_deleted_channel_leaves_the_sidebar() {
    let mut s = seeded();
    let mut gone = channel("c1", 20, 16);
    gone.delete_at = Timestamp(900);
    gone.update_at = Timestamp(900);
    s.apply_channel(gone);
    assert!(!s.sidebar().iter().any(|r| r.channel.as_str() == "c1"));
}

#[test]
fn a_channel_without_a_membership_is_not_in_the_sidebar() {
    let mut s = store();
    s.apply_channels(vec![channel("c1", 0, 0)]);
    assert!(s.sidebar().is_empty());
}

/// A DM's name is the two user ids joined by `__` and its `display_name` is
/// empty, so the label has to come from the roster.
#[test]
fn a_dm_is_labelled_with_the_other_participant() {
    let mut s = store();
    let dm = Channel {
        id: ChannelId::new("d1"),
        kind: ChannelType::Direct,
        name: format!("{ME}__u-jane"),
        display_name: String::new(),
        ..Channel::default()
    };
    s.apply_channels(vec![dm.clone()]);
    s.apply_members(vec![member("d1", 0, 0, 0)]);

    // Before the roster arrives, the raw id is shown rather than a blank row.
    assert_eq!(s.channel_label(&dm), "u-jane");

    s.apply_users(vec![User {
        id: UserId::new("u-jane"),
        username: "jane".into(),
        nickname: "Jane".into(),
        ..User::default()
    }]);
    // The server's default format is `username`, and this follows it rather
    // than preferring the nickname the way it once did.
    assert_eq!(s.channel_label(&dm), "jane");

    s.name_format = mm_types::NameFormat::NicknameFullName;
    assert_eq!(s.channel_label(&dm), "Jane");
}

#[test]
fn a_channel_with_no_display_name_falls_back_to_its_url_name() {
    let s = store();
    let c = Channel {
        id: ChannelId::new("c1"),
        name: "town-square".into(),
        display_name: String::new(),
        ..Channel::default()
    };
    assert_eq!(s.channel_label(&c), "town-square");
}

#[test]
fn sidebar_rows_report_unread_according_to_mute_state() {
    let mut s = store();
    s.apply_channels(vec![channel("c1", 9, 9)]);
    let mut muted = member("c1", 0, 0, 0);
    muted
        .notify_props
        .insert("mark_unread".into(), "mention".into());
    s.apply_members(vec![muted]);

    let row = &s.sidebar()[0];
    assert!(row.muted);
    assert!(!row.is_unread(), "a muted channel shows no unread marker");
    assert!(!s.any_unread());
}

// ------------------------------------------------------------ misc

#[test]
fn users_and_teams_are_stored() {
    let mut s = store();
    let changes = s.apply_users(vec![User {
        id: UserId::new("u1"),
        username: "jane".into(),
        ..User::default()
    }]);
    assert_eq!(changes, vec![Change::User(UserId::new("u1"))]);

    s.apply_teams(vec![Team {
        id: mm_types::TeamId::new("t1"),
        display_name: "Alpha".into(),
        ..Team::default()
    }]);
    assert_eq!(s.teams.len(), 1);

    let changes = s.apply_ws(&WsEvent::UserUpdated(User {
        id: UserId::new("u1"),
        username: "janet".into(),
        ..User::default()
    }));
    assert_eq!(changes, vec![Change::User(UserId::new("u1"))]);
    assert_eq!(s.users[&UserId::new("u1")].username, "janet");
}

/// Presence, typing and unrecognised events carry no state this store owns.
/// Returning no changes is correct, not a stub.
#[test]
fn events_the_store_does_not_model_produce_no_changes() {
    let mut s = seeded();
    for event in [
        WsEvent::StatusChange {
            user_id: "u1".into(),
            status: "online".into(),
        },
        WsEvent::Typing {
            channel_id: ChannelId::new("c1"),
            user_id: "u1".into(),
            parent_id: String::new(),
        },
        WsEvent::Unknown {
            event: "something_new".into(),
        },
    ] {
        assert!(s.apply_ws(&event).is_empty(), "{event:?}");
    }
}

#[test]
fn every_derive_is_exercised() {
    let s = seeded();
    assert!(!format!("{s:?}").is_empty());
    let cloned = s.clone();
    assert_eq!(cloned.teams.len(), s.teams.len());

    let row = &s.sidebar()[0];
    assert_eq!(row, &row.clone());
    assert!(!format!("{row:?}").is_empty());

    let c = Change::Sidebar;
    assert_eq!(c, c.clone());
    assert!(!format!("{c:?}").is_empty());
    assert!(Change::Sidebar < Change::ReadState);
    assert_eq!(Store::default().me, UserId::default());
    assert_eq!(Threading::default(), Threading::Flat);
}

// ------------------------------------------------------------ edge cases
//
// Everything below is a path the gate found untested. Each is a shape the
// server or the GUI can genuinely produce, and each would otherwise be a
// branch nothing had ever executed.

/// A membership for another user omits `last_update_at` entirely, so the
/// freshness comparison has to cope with it being absent on either side
/// rather than assuming both are present.
#[test]
fn a_member_push_without_a_timestamp_is_not_treated_as_stale() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");

    let mut no_stamp = member("c1", 13, 11, 7);
    no_stamp.last_update_at = None;
    s.apply_member(no_stamp);
    assert_eq!(
        s.unread(&c1).mentions,
        7,
        "absent freshness must not block the update"
    );
}

/// `last_viewed_at` is absent on a foreign membership, so merging has to keep
/// whichever side actually has a value instead of discarding both.
#[test]
fn last_viewed_at_survives_a_push_that_omits_it() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");

    let mut no_viewed = member("c1", 13, 11, 1);
    no_viewed.last_viewed_at = None;
    s.apply_member(no_viewed);
    assert_eq!(
        s.my_member[&c1].last_viewed_at,
        Some(Timestamp(100)),
        "the held value must be kept"
    );

    // And the reverse: a held None takes the incoming value.
    let mut fresh = store();
    let mut without = member("c1", 0, 0, 0);
    without.last_viewed_at = None;
    fresh.apply_member(without);
    let mut with = member("c1", 0, 0, 0);
    with.last_viewed_at = Some(Timestamp(42));
    with.last_update_at = Some(Timestamp(200));
    fresh.apply_member(with);
    assert_eq!(fresh.my_member[&c1].last_viewed_at, Some(Timestamp(42)));
}

/// The authoritative read-state push, which arrives as a WebSocket event
/// rather than through a REST fetch.
#[test]
fn a_channel_member_updated_event_corrects_the_unread_state() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    assert_eq!(s.unread(&c1).mentions, 1);

    let mut pushed = member("c1", 20, 16, 0);
    pushed.last_update_at = Some(Timestamp(900));
    let changes = s.apply_ws(&WsEvent::ChannelMemberUpdated(pushed));

    assert_eq!(changes, vec![Change::Unread(c1.clone())]);
    assert_eq!(s.unread(&c1), Unread::default());
}

/// Re-fetching the page already held must report no change, and a page whose
/// adjacency cannot be established must not be merged.
#[test]
fn a_page_that_adds_nothing_and_a_page_that_would_gap_both_leave_the_timeline_alone() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let first = page(vec![post("p1", "c1", 10)]);
    s.apply_post_list(&c1, &first, PageAnchor::Newest);
    assert_eq!(s.channel_posts(&c1).len(), 1);

    s.apply_post_list(&c1, &first, PageAnchor::Newest);
    assert_eq!(s.channel_posts(&c1).len(), 1, "re-merging adds nothing");

    let disjoint = page(vec![post("p99", "c1", 99_000)]);
    s.apply_post_list(&c1, &disjoint, PageAnchor::Unanchored);
    assert_eq!(
        s.channel_posts(&c1).len(),
        1,
        "a page that would leave a gap must not enter the timeline"
    );
    assert!(
        s.posts.contains_key(&PostId::new("p99")),
        "but the post itself is still stored, since a thread root pulled in by \
         ?since= is wanted even when it does not belong in the ordering"
    );
}

/// A post can arrive for a channel the client has not fetched yet — a DM
/// opened by somebody else, for instance. It must be stored without panicking,
/// even though there is no channel record to raise a counter on.
#[test]
fn a_post_for_an_unknown_channel_is_stored_without_a_counter_bump() {
    let mut s = seeded();
    let changes = s.apply_ws(&WsEvent::Posted {
        post: post("p1", "brand-new", 10),
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });
    assert_eq!(changes, vec![Change::Post(PostId::new("p1"))]);
    assert!(s.posts.contains_key(&PostId::new("p1")));
    assert_eq!(s.channel_posts(&ChannelId::new("brand-new")).len(), 1);
}

#[test]
fn deleting_a_post_in_a_channel_with_no_timeline_is_harmless() {
    let mut s = seeded();
    let changes = s.apply_ws(&WsEvent::PostDeleted(post("p1", "never-fetched", 10)));
    assert_eq!(changes, vec![Change::PostRemoved(PostId::new("p1"))]);
    assert!(s.posts[&PostId::new("p1")].is_deleted());
}

/// A delete event whose post carries no `delete_at` still has to produce a
/// tombstone, or the post would silently stay visible.
#[test]
fn a_delete_event_without_a_delete_timestamp_still_tombstones() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);

    let mut bare = post("p1", "c1", 10);
    bare.delete_at = Timestamp::ZERO;
    bare.update_at = Timestamp::ZERO;
    s.apply_ws(&WsEvent::PostDeleted(bare));

    assert!(s.posts[&PostId::new("p1")].is_deleted());
    assert!(s.channel_posts(&c1).is_empty());
}

#[test]
fn marking_an_unjoined_channel_read_is_harmless() {
    let mut s = seeded();
    let unknown = ChannelId::new("not-mine");
    assert_eq!(
        s.mark_read_locally(&unknown),
        vec![Change::Unread(unknown.clone())]
    );
    assert_eq!(s.unread(&unknown), Unread::default());
}

/// The first read-state push for a channel can arrive before any membership
/// with a `last_viewed_at`, so the incoming timestamp has to be taken as-is.
#[test]
fn viewing_a_channel_with_no_recorded_view_time_takes_the_pushed_time() {
    let mut s = store();
    s.apply_channels(vec![channel("c1", 3, 3)]);
    let mut without = member("c1", 0, 0, 0);
    without.last_viewed_at = None;
    s.apply_member(without);

    let mut times = BTreeMap::new();
    times.insert("c1".to_owned(), 777_i64);
    s.apply_ws(&WsEvent::MultipleChannelsViewed {
        channel_times: times,
    });

    assert_eq!(
        s.my_member[&ChannelId::new("c1")].last_viewed_at,
        Some(Timestamp(777))
    );
}

/// A page that adds posts but whose `prev_post_id` is non-empty means older
/// messages exist that the client does not hold, so the start of the channel
/// must NOT be marked reached — otherwise "load more" disappears and the
/// history looks complete when it is not.
#[test]
fn a_page_with_more_history_behind_it_does_not_mark_the_start_reached() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let mut p = page(vec![post("p5", "c1", 50)]);
    p.prev_post_id = "p4".to_owned();

    s.apply_post_list(&c1, &p, PageAnchor::Newest);
    assert_eq!(s.channel_posts(&c1).len(), 1);
    assert!(
        !s.timelines[&c1].older_complete(),
        "the server said there is more behind this page"
    );
}

/// When the delete event already carries a timestamp, it is kept rather than
/// replaced — the server's own time is better than one we synthesize.
#[test]
fn a_delete_event_that_carries_a_timestamp_keeps_it() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    s.apply_post_list(&c1, &page(vec![post("p1", "c1", 10)]), PageAnchor::Newest);

    let mut deleted = post("p1", "c1", 10);
    deleted.delete_at = Timestamp(4242);
    s.apply_ws(&WsEvent::PostDeleted(deleted));

    assert_eq!(
        s.posts[&PostId::new("p1")].delete_at,
        Timestamp(4242),
        "the server's own delete time must be preserved"
    );
    assert!(s.channel_posts(&c1).is_empty());
}

/// Under collapsed reply threads a reply must not appear in the channel. The
/// server maintains root-only counters precisely so replies do not make a
/// channel unread, so showing them here makes the message list disagree with
/// the badge beside it.
#[test]
fn collapsed_threading_keeps_replies_out_of_the_channel() {
    let mut s = seeded();
    s.threading = Threading::Collapsed;
    let c1 = ChannelId::new("c1");

    let mut reply = post("p2", "c1", 20);
    reply.root_id = PostId::new("p1");
    s.apply_post_list(
        &c1,
        &page(vec![post("p1", "c1", 10), reply, post("p3", "c1", 30)]),
        PageAnchor::Newest,
    );

    let shown: Vec<&str> = s.channel_posts(&c1).iter().map(|p| p.id.as_str()).collect();
    assert_eq!(shown, vec!["p1", "p3"], "the reply belongs in its thread");
}

/// With threads flat, every post belongs in the channel — that is the whole
/// difference between the two modes.
#[test]
fn flat_threading_shows_replies_in_the_channel() {
    let mut s = seeded();
    s.threading = Threading::Flat;
    let c1 = ChannelId::new("c1");

    let mut reply = post("p2", "c1", 20);
    reply.root_id = PostId::new("p1");
    s.apply_post_list(
        &c1,
        &page(vec![post("p1", "c1", 10), reply]),
        PageAnchor::Newest,
    );

    assert_eq!(s.channel_posts(&c1).len(), 2);
}

/// The server's own count includes replies never fetched, so it wins — but a
/// thread built up live over the WebSocket has a root whose stated count is
/// stale, and the local tally covers that.
#[test]
fn reply_count_prefers_the_servers_number_but_never_undercounts() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let root = PostId::new("p1");

    let mut stated = post("p1", "c1", 10);
    stated.reply_count = 9;
    s.apply_post_list(&c1, &page(vec![stated]), PageAnchor::Newest);
    assert_eq!(s.reply_count(&root), 9, "the server knows about more");

    // Two replies arrive live onto a root that claims none.
    let mut bare = post("p5", "c1", 50);
    bare.reply_count = 0;
    s.apply_post_list(&c1, &page(vec![bare]), PageAnchor::Newest);
    for (id, at) in [("p6", 60), ("p7", 70)] {
        let mut reply = post(id, "c1", at);
        reply.root_id = PostId::new("p5");
        s.apply_ws(&WsEvent::Posted {
            post: reply,
            channel_display_name: String::new(),
            sender_name: String::new(),
            team_id: String::new(),
            mentions: Vec::new(),
        });
    }
    assert_eq!(s.reply_count(&PostId::new("p5")), 2, "counted locally");

    assert_eq!(s.reply_count(&PostId::new("absent")), 0);
}

/// A deleted reply must not be counted, or a thread shows replies that are
/// not there.
#[test]
fn reply_count_ignores_deleted_replies() {
    let mut s = seeded();
    let c1 = ChannelId::new("c1");
    let mut root = post("p1", "c1", 10);
    root.reply_count = 0;
    let mut reply = post("p2", "c1", 20);
    reply.root_id = PostId::new("p1");
    s.apply_post_list(&c1, &page(vec![root, reply.clone()]), PageAnchor::Newest);
    assert_eq!(s.reply_count(&PostId::new("p1")), 1);

    s.apply_ws(&WsEvent::PostDeleted(reply));
    assert_eq!(s.reply_count(&PostId::new("p1")), 0);
}

fn category_list(pairs: &[(&str, &str, &[&str])]) -> mm_types::CategoryList {
    mm_types::CategoryList {
        order: pairs.iter().map(|(id, _, _)| (*id).to_owned()).collect(),
        // Deliberately built in the reverse of `order`: the server does not
        // promise the array is in stacking order, and reading it as if it
        // were is the bug this guards.
        categories: pairs
            .iter()
            .rev()
            .map(|(id, name, channels)| mm_types::ChannelCategory {
                id: (*id).to_owned(),
                display_name: (*name).to_owned(),
                channel_ids: channels.iter().map(|c| ChannelId::new(*c)).collect(),
                ..mm_types::ChannelCategory::default()
            })
            .collect(),
    }
}

/// The server sends the whole category list every time, in `order`, and the
/// store keeps it in that order so the view does not have to re-derive it.
#[test]
fn categories_are_stored_in_the_servers_stacking_order() {
    let mut store = store();
    let list = category_list(&[("fav", "Favorites", &["c2"]), ("ch", "Channels", &["c1"])]);
    assert_eq!(store.apply_categories(&list), vec![Change::Sidebar]);
    let stacked: Vec<&str> = store
        .categories
        .iter()
        .map(|c| c.display_name.as_str())
        .collect();
    assert_eq!(stacked, ["Favorites", "Channels"]);

    // A later fetch replaces the list outright: a category the user deleted
    // has to disappear, and there is no version on it to join against.
    store.apply_categories(&category_list(&[("ch", "Channels", &["c1"])]));
    assert_eq!(store.categories.len(), 1);
}

/// The sections projection reads the store's own channels and membership,
/// so the view asks one question and gets the finished sidebar.
#[test]
fn the_sidebar_projects_into_the_servers_sections() {
    let mut store = store();
    store.apply_channels(vec![channel("c1", 0, 0), channel("c2", 0, 0)]);
    store.apply_members(vec![member("c1", 0, 0, 0), member("c2", 0, 0, 0)]);
    store.apply_categories(&category_list(&[
        ("fav", "Favorites", &["c2"]),
        ("ch", "Channels", &["c1"]),
    ]));

    let sections = store.sidebar_sections();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].display_name, "Favorites");
    assert_eq!(sections[0].rows[0].channel, ChannelId::new("c2"));
    assert_eq!(sections[1].rows[0].channel, ChannelId::new("c1"));
}

/// Before the first sync there are no categories, and the sidebar still has
/// to draw: an empty window on every start would be worse than an ungrouped
/// list for the second it takes to arrive.
#[test]
fn the_sidebar_still_draws_before_any_categories_arrive() {
    let mut store = store();
    store.apply_channels(vec![channel("c1", 0, 0)]);
    store.apply_members(vec![member("c1", 0, 0, 0)]);
    let sections = store.sidebar_sections();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].rows.len(), 1);
}

/// The channel header asks one question and gets both halves of its answer.
#[test]
fn the_channel_heading_carries_the_name_and_the_member_count() {
    let mut store = store();
    store.apply_channels(vec![channel("c1", 0, 0)]);
    let id = ChannelId::new("c1");

    // Before the stats arrive there is a name and no count, and the header
    // has to draw anyway.
    let before = store.channel_heading(&id);
    assert_eq!(before.name, "Channel c1");
    assert_eq!(before.members, None);

    let changes = store.apply_stats(mm_types::ChannelStats {
        channel_id: id.clone(),
        member_count: 7,
        ..mm_types::ChannelStats::default()
    });
    assert_eq!(changes, vec![Change::Unread(id.clone())]);
    assert_eq!(store.channel_heading(&id).members, Some(7));
}

/// A channel we have never loaded still gets a heading rather than a blank
/// one -- the same reason `channel_label` falls back to the raw id.
#[test]
fn an_unknown_channel_still_gets_a_heading() {
    let store = store();
    let heading = store.channel_heading(&ChannelId::new("c-unknown"));
    assert_eq!(heading.name, "c-unknown");
    assert_eq!(heading.members, None);
}

fn reaction(post: &str, user: &str, emoji: &str, at: i64) -> mm_types::Reaction {
    mm_types::Reaction {
        post_id: PostId::new(post),
        user_id: UserId::new(user),
        emoji_name: emoji.to_owned(),
        create_at: Timestamp(at),
        delete_at: Timestamp::ZERO,
    }
}

/// Reactions ride in a post's metadata, and the store has to pick them up
/// from a page as well as from the WebSocket.
#[test]
fn reactions_arrive_with_the_post_that_carries_them() {
    let mut store = store();
    let post: Post = serde_json::from_str(
        r#"{"id":"p1","channel_id":"c1","create_at":10,"update_at":10,
            "metadata":{"reactions":[
              {"post_id":"p1","user_id":"u-other","emoji_name":"tada","create_at":20},
              {"post_id":"p1","user_id":"u-me","emoji_name":"tada","create_at":30}]}}"#,
    )
    .expect("parses");
    store.apply_ws(&WsEvent::Posted {
        post,
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });

    let summary = store.reactions_for(&PostId::new("p1"));
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].emoji, "tada");
    assert_eq!(summary[0].count, 2);
    assert!(summary[0].mine, "the signed-in user is one of the two");
}

/// A reaction the server has soft-deleted must not be counted.
#[test]
fn a_deleted_reaction_in_metadata_is_not_counted() {
    let mut store = store();
    let post: Post = serde_json::from_str(
        r#"{"id":"p1","channel_id":"c1","create_at":10,"update_at":10,
            "metadata":{"reactions":[
              {"post_id":"p1","user_id":"u-other","emoji_name":"tada","create_at":20,
               "delete_at":25}]}}"#,
    )
    .expect("parses");
    store.apply_ws(&WsEvent::Posted {
        post,
        channel_display_name: String::new(),
        sender_name: String::new(),
        team_id: String::new(),
        mentions: Vec::new(),
    });
    assert!(store.reactions_for(&PostId::new("p1")).is_empty());
}

/// Adding and removing over the WebSocket, including the duplicate that a
/// reconnect replay delivers.
#[test]
fn reactions_add_and_remove_and_survive_a_replay() {
    let mut store = store();
    let added = reaction("p1", "u-other", "tada", 20);

    let changes = store.apply_ws(&WsEvent::ReactionAdded(added.clone()));
    assert_eq!(changes, vec![Change::Post(PostId::new("p1"))]);
    // The dead queue replays the same event after a reconnect.
    store.apply_ws(&WsEvent::ReactionAdded(added.clone()));
    assert_eq!(store.reactions_for(&PostId::new("p1"))[0].count, 1);
    assert!(!store.reactions_for(&PostId::new("p1"))[0].mine);

    store.apply_ws(&WsEvent::ReactionRemoved(added.clone()));
    assert!(store.reactions_for(&PostId::new("p1")).is_empty());
    // And removing one that was never there is not an error -- neither when
    // the post is known and the reaction is not, nor when the post itself
    // has never been seen. A reconnect replays removals too.
    store.apply_ws(&WsEvent::ReactionRemoved(added));
    assert!(store.reactions_for(&PostId::new("p1")).is_empty());
    store.apply_ws(&WsEvent::ReactionRemoved(reaction(
        "p-never-seen",
        "u-other",
        "tada",
        40,
    )));
    assert!(store.reactions_for(&PostId::new("p-never-seen")).is_empty());
}

/// Emoji are shown in the order they first appeared on the post. Sorting by
/// name or by count instead would reshuffle the row every time somebody
/// joined an existing reaction, under the cursor of whoever was clicking.
#[test]
fn emoji_keep_the_order_they_first_appeared_in() {
    let mut store = store();
    store.apply_ws(&WsEvent::ReactionAdded(reaction("p1", "u-a", "zzz", 10)));
    store.apply_ws(&WsEvent::ReactionAdded(reaction("p1", "u-b", "aaa", 20)));
    store.apply_ws(&WsEvent::ReactionAdded(reaction("p1", "u-c", "aaa", 30)));
    store.apply_ws(&WsEvent::ReactionAdded(reaction("p1", "u-d", "aaa", 5)));

    let summary = store.reactions_for(&PostId::new("p1"));
    let order: Vec<(&str, i64)> = summary
        .iter()
        .map(|r| (r.emoji.as_str(), r.count))
        .collect();
    // `aaa` first because its earliest reaction is at 5, even though it was
    // the second emoji this test added.
    assert_eq!(order, [("aaa", 3), ("zzz", 1)]);
}

/// A post nobody has reacted to has no reactions, and asking about a post
/// that does not exist is not an error.
#[test]
fn a_post_with_no_reactions_has_none() {
    let store = store();
    assert!(store.reactions_for(&PostId::new("p-nothing")).is_empty());
}

fn dm_channel(id: &str, other: &str) -> Channel {
    Channel {
        id: ChannelId::new(id),
        kind: ChannelType::Direct,
        name: format!("{ME}__{other}"),
        ..Channel::default()
    }
}

fn person(id: &str, username: &str, nickname: &str) -> User {
    User {
        id: UserId::new(id),
        username: username.to_owned(),
        nickname: nickname.to_owned(),
        ..User::default()
    }
}

/// Two colleagues who both go by "Alex" are two identical rows, and an
/// unread badge on one of them tells the user nothing.
///
/// Only reachable under a format that can collide: usernames are unique on
/// a server by construction, so under the default there is nothing to tell
/// apart.
#[test]
fn two_people_with_the_same_display_name_are_told_apart_by_username() {
    let mut store = store();
    store.name_format = mm_types::NameFormat::NicknameFullName;
    store.apply_users(vec![
        person("u-1", "alex.m", "Alex"),
        person("u-2", "alex.t", "Alex"),
        person("u-3", "sam.q", "Sam"),
    ]);
    store.apply_channels(vec![
        dm_channel("d1", "u-1"),
        dm_channel("d2", "u-2"),
        // Sam is in the same sidebar and must come through untouched: only
        // the names that actually collide are tagged.
        dm_channel("d3", "u-3"),
    ]);
    store.apply_members(vec![
        member("d1", 0, 0, 0),
        member("d2", 0, 0, 0),
        member("d3", 0, 0, 0),
    ]);

    let names: Vec<String> = store
        .sidebar()
        .into_iter()
        .map(|r| r.display_name)
        .collect();
    assert_eq!(names, ["Alex (@alex.m)", "Alex (@alex.t)", "Sam"]);
}

/// Only the duplicates are touched. Tagging every DM with a username is
/// noise on the rows that were never ambiguous.
#[test]
fn an_unambiguous_name_is_left_alone() {
    let mut store = store();
    store.name_format = mm_types::NameFormat::NicknameFullName;
    store.apply_users(vec![
        person("u-1", "alex.m", "Alex"),
        person("u-2", "sam.t", "Sam"),
    ]);
    store.apply_channels(vec![dm_channel("d1", "u-1"), dm_channel("d2", "u-2")]);
    store.apply_members(vec![member("d1", 0, 0, 0), member("d2", 0, 0, 0)]);

    let names: Vec<String> = store
        .sidebar()
        .into_iter()
        .map(|r| r.display_name)
        .collect();
    assert_eq!(names, ["Alex", "Sam"]);
}

/// The same channel name in two teams is the same problem with a different
/// answer: the team is what tells them apart, not a username.
#[test]
fn a_channel_name_shared_across_teams_is_told_apart_by_team() {
    let mut store = store();
    store.apply_teams(vec![
        Team {
            id: TeamId::new("t1"),
            display_name: "Platform".into(),
            ..Team::default()
        },
        Team {
            id: TeamId::new("t2"),
            display_name: "Support".into(),
            ..Team::default()
        },
    ]);
    let mut a = channel("c1", 0, 0);
    a.display_name = "General".into();
    a.team_id = TeamId::new("t1");
    let mut b = channel("c2", 0, 0);
    b.display_name = "General".into();
    b.team_id = TeamId::new("t2");
    store.apply_channels(vec![a, b]);
    store.apply_members(vec![member("c1", 0, 0, 0), member("c2", 0, 0, 0)]);

    let names: Vec<String> = store
        .sidebar()
        .into_iter()
        .map(|r| r.display_name)
        .collect();
    assert_eq!(names, ["General (Platform)", "General (Support)"]);
}

/// With nothing to tell them apart -- an unknown team, a roster that has not
/// loaded -- the rows stay as they were rather than growing an empty suffix.
#[test]
fn a_duplicate_with_nothing_to_distinguish_it_is_left_as_it_is() {
    let mut store = store();
    let mut a = channel("c1", 0, 0);
    a.display_name = "General".into();
    a.team_id = TeamId::new("t-unknown");
    let mut b = channel("c2", 0, 0);
    b.display_name = "General".into();
    b.team_id = TeamId::new("t-unknown");
    store.apply_channels(vec![a, b]);
    store.apply_members(vec![member("c1", 0, 0, 0), member("c2", 0, 0, 0)]);

    let names: Vec<String> = store
        .sidebar()
        .into_iter()
        .map(|r| r.display_name)
        .collect();
    assert_eq!(names, ["General", "General"]);
}

/// A one-to-one DM names the person on the other end, so the sidebar can
/// draw their face. Everything else has no single other person.
#[test]
fn a_dm_row_names_the_other_party() {
    let mut store = store();
    store.apply_channels(vec![dm_channel("d1", "u-1"), channel("c1", 0, 0)]);
    store.apply_members(vec![member("d1", 0, 0, 0), member("c1", 0, 0, 0)]);

    let rows = store.sidebar();
    let dm = rows.iter().find(|r| r.channel.as_str() == "d1").unwrap();
    assert_eq!(dm.avatar_user, Some(UserId::new("u-1")));
    let open = rows.iter().find(|r| r.channel.as_str() == "c1").unwrap();
    assert_eq!(open.avatar_user, None);
}

/// The view stops asking for older pages once the channel's history is
/// complete; without this it would re-request the first page forever at the
/// top of the scrollback.
#[test]
fn a_channel_reports_whether_its_history_is_complete() {
    let mut store = store();
    let id = ChannelId::new("c1");
    // Nothing loaded: there is certainly more to fetch.
    assert!(!store.older_complete(&id));

    // A page with no `prev_post_id` is the beginning of the channel.
    let mut posts = std::collections::HashMap::new();
    posts.insert(PostId::new("p1"), post("p1", "c1", 10));
    let page = PostList {
        order: vec![PostId::new("p1")],
        posts,
        // Empty means there is nothing before this page.
        prev_post_id: String::new(),
        ..PostList::default()
    };
    store.apply_post_list(&id, &page, PageAnchor::Newest);
    assert!(store.older_complete(&id));
}

/// A thread reads as its root followed by the replies, oldest first --
/// which is not the order a `HashMap` would hand them over in.
#[test]
fn a_thread_is_its_root_then_its_replies_in_order() {
    let mut store = store();
    let root = post("p-root", "c1", 10);
    let mut later = post("p-late", "c1", 30);
    later.root_id = PostId::new("p-root");
    let mut earlier = post("p-early", "c1", 20);
    earlier.root_id = PostId::new("p-root");
    // A reply to something else entirely, which must not be swept in.
    let mut elsewhere = post("p-other", "c1", 25);
    elsewhere.root_id = PostId::new("p-different");

    let mut page = PostList::default();
    for p in [root, later, earlier, elsewhere] {
        page.order.push(p.id.clone());
        page.posts.insert(p.id.clone(), p);
    }
    store.apply_post_list(&ChannelId::new("c1"), &page, PageAnchor::Newest);

    let ids: Vec<&str> = store
        .thread_posts(&PostId::new("p-root"))
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids, ["p-root", "p-early", "p-late"]);
}

/// A deleted reply disappears; a deleted root does not take its thread with
/// it, because the replies are still worth reading.
#[test]
fn a_deleted_reply_goes_but_a_deleted_root_keeps_its_thread() {
    let mut store = store();
    let mut root = post("p-root", "c1", 10);
    root.delete_at = Timestamp(50);
    let mut gone = post("p-gone", "c1", 20);
    gone.root_id = PostId::new("p-root");
    gone.delete_at = Timestamp(40);
    let mut kept = post("p-kept", "c1", 30);
    kept.root_id = PostId::new("p-root");

    let mut page = PostList::default();
    for p in [root, gone, kept] {
        page.order.push(p.id.clone());
        page.posts.insert(p.id.clone(), p);
    }
    store.apply_post_list(&ChannelId::new("c1"), &page, PageAnchor::Newest);

    let ids: Vec<&str> = store
        .thread_posts(&PostId::new("p-root"))
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids, ["p-root", "p-kept"]);
}

/// A post nobody replied to is a thread of one, and an unknown root is
/// nothing at all rather than a panic.
#[test]
fn a_thread_of_one_and_a_thread_of_none() {
    let mut store = store();
    let mut page = PostList::default();
    let only = post("p1", "c1", 10);
    page.order.push(only.id.clone());
    page.posts.insert(only.id.clone(), only);
    store.apply_post_list(&ChannelId::new("c1"), &page, PageAnchor::Newest);

    assert_eq!(store.thread_posts(&PostId::new("p1")).len(), 1);
    assert!(store.thread_posts(&PostId::new("p-nothing")).is_empty());
}

/// A thread page is not a page of a channel: with collapsed threads its
/// replies are in no channel timeline at all, so merging it as one would
/// claim an adjacency the server never stated. The posts land, the timeline
/// does not move.
#[test]
fn a_thread_page_stores_its_posts_without_touching_the_timeline() {
    let mut store = store();
    let channel = ChannelId::new("c1");

    let mut first = PostList::default();
    let root = post("p-root", "c1", 10);
    first.order.push(root.id.clone());
    first.posts.insert(root.id.clone(), root);
    store.apply_post_list(&channel, &first, PageAnchor::Newest);
    let timeline_before = store.channel_posts(&channel).len();

    let mut thread = PostList::default();
    let root_again = post("p-root", "c1", 10);
    let mut reply = post("p-reply", "c1", 20);
    reply.root_id = PostId::new("p-root");
    for p in [root_again, reply] {
        thread.order.push(p.id.clone());
        thread.posts.insert(p.id.clone(), p);
    }
    let changes = store.apply_thread(&thread);

    // The reply is new, so it is reported; the root is unchanged, so it is
    // not. And the channel is exactly as it was.
    assert_eq!(changes, vec![Change::Post(PostId::new("p-reply"))]);
    assert_eq!(store.channel_posts(&channel).len(), timeline_before);
    assert_eq!(store.thread_posts(&PostId::new("p-root")).len(), 2);
}

/// Opening a channel moves the last-viewed time forward, so the window has
/// to capture it first -- otherwise the "new messages" line would vanish in
/// the instant it became useful.
#[test]
fn the_last_viewed_time_is_readable_and_moves_on_opening() {
    let mut store = store();
    let id = ChannelId::new("c1");
    assert_eq!(store.last_viewed(&id), None);

    store.apply_channels(vec![channel("c1", 10, 10)]);
    store.apply_members(vec![member("c1", 3, 3, 0)]);
    let before = store.last_viewed(&id).expect("a membership was applied");

    store.mark_read_locally(&id);
    let after = store.last_viewed(&id).expect("still a member");
    assert!(after >= before, "{after:?} went backwards from {before:?}");
}

/// Under the server's default there is nothing to disambiguate: a username
/// is unique on a server by construction, so two rows can never read the
/// same and nothing is appended to either.
#[test]
fn usernames_never_need_disambiguating() {
    let mut store = store();
    store.apply_users(vec![
        person("u-1", "alex.m", "Alex"),
        person("u-2", "alex.t", "Alex"),
    ]);
    store.apply_channels(vec![dm_channel("d1", "u-1"), dm_channel("d2", "u-2")]);
    store.apply_members(vec![member("d1", 0, 0, 0), member("d2", 0, 0, 0)]);

    let names: Vec<String> = store
        .sidebar()
        .into_iter()
        .map(|r| r.display_name)
        .collect();
    assert_eq!(names, ["alex.m", "alex.t"]);
}

/// A webhook posts under a name of its own choosing, and that is what
/// belongs on the author line: one bot account sends alerts as
/// "Alertmanager", deploys as "Deploys" and builds as "CI".
#[test]
fn a_webhooks_chosen_name_wins_over_its_account() {
    let mut store = store();
    store.apply_users(vec![person("u-bot", "turing", "")]);

    let mut alert = post("p1", "c1", 10);
    alert.user_id = UserId::new("u-bot");
    alert.props = serde_json::json!({
        "from_webhook": "true",
        "override_username": "Alertmanager",
    });
    assert_eq!(
        store.post_author(&alert),
        ("Alertmanager".to_owned(), true),
        "the account is `turing`, which is not what the alert is from"
    );
}

/// A bot account posting as itself is still a bot, and an ordinary person
/// is not.
#[test]
fn a_bot_account_is_marked_and_a_person_is_not() {
    let mut store = store();
    let mut robot = person("u-bot", "buildbot", "");
    robot.is_bot = true;
    store.apply_users(vec![robot, person("u-her", "jane", "")]);

    let mut from_bot = post("p1", "c1", 10);
    from_bot.user_id = UserId::new("u-bot");
    assert_eq!(store.post_author(&from_bot), ("buildbot".to_owned(), true));

    let mut from_person = post("p2", "c1", 20);
    from_person.user_id = UserId::new("u-her");
    assert_eq!(store.post_author(&from_person), ("jane".to_owned(), false));
}

/// The roster behind mention highlighting: only real usernames light up,
/// the reader's own is marked apart, and the three group names are always
/// mentions even though nobody is called that.
#[test]
fn mentions_resolve_against_the_roster() {
    let mut store = store();
    store.apply_users(vec![person(ME, "legion", ""), person("u-2", "alukard", "")]);

    assert_eq!(store.mentioned("legion"), Some(mm_md::Mentioned::Me));
    assert_eq!(store.mentioned("alukard"), Some(mm_md::Mentioned::Someone));
    assert_eq!(store.mentioned("nobody"), None);
    for group in ["all", "channel", "here"] {
        assert_eq!(store.mentioned(group), Some(mm_md::Mentioned::Someone));
    }
}

/// `/stats` counts only active members, so a group DM of five with two
/// deactivated people reported three -- under a title listing all five.
/// The participants are the names in that title.
#[test]
fn a_group_dm_is_counted_by_the_names_in_its_title() {
    let mut store = store();
    let mut group = channel("g1", 0, 0);
    group.kind = ChannelType::Group;
    group.display_name = "aiksandr, diogen, legion".into();
    store.apply_channels(vec![group]);
    store.apply_stats(mm_types::ChannelStats {
        channel_id: ChannelId::new("g1"),
        // What the server said, and what must not be shown.
        member_count: 2,
        ..mm_types::ChannelStats::default()
    });

    let heading = store.channel_heading(&ChannelId::new("g1"));
    assert_eq!(heading.name, "aiksandr, diogen, legion");
    assert_eq!(heading.members, Some(3));
}

/// A one-to-one DM gets no count: "2 members" over somebody's name tells
/// the reader nothing, and Mattermost shows none either.
#[test]
fn a_direct_message_has_no_member_count() {
    let mut store = store();
    store.apply_users(vec![person("u-1", "jane", "")]);
    store.apply_channels(vec![dm_channel("d1", "u-1")]);
    store.apply_stats(mm_types::ChannelStats {
        channel_id: ChannelId::new("d1"),
        member_count: 2,
        ..mm_types::ChannelStats::default()
    });

    let heading = store.channel_heading(&ChannelId::new("d1"));
    assert_eq!(heading.name, "jane");
    assert_eq!(heading.members, None);
}

/// An ordinary channel still uses the server's count, which is what that
/// number means there.
#[test]
fn an_ordinary_channel_uses_the_servers_count() {
    let mut store = store();
    store.apply_channels(vec![channel("c1", 0, 0)]);
    store.apply_stats(mm_types::ChannelStats {
        channel_id: ChannelId::new("c1"),
        member_count: 34,
        ..mm_types::ChannelStats::default()
    });
    assert_eq!(
        store.channel_heading(&ChannelId::new("c1")).members,
        Some(34)
    );
}

/// A group DM with no title yet shows no count rather than one.
#[test]
fn a_group_dm_with_no_title_has_no_count() {
    let mut store = store();
    let mut group = channel("g2", 0, 0);
    group.kind = ChannelType::Group;
    group.display_name = String::new();
    store.apply_channels(vec![group]);
    assert_eq!(store.channel_heading(&ChannelId::new("g2")).members, None);
}

/// Mattermost shows the channel's own header line beside its name, and it
/// is markdown -- people put links in it. Only the first line: a title bar
/// has one, and a header can run to paragraphs.
#[test]
fn the_heading_carries_the_channels_header_line() {
    let mut store = store();
    let mut with_header = channel("c1", 0, 0);
    with_header.header = "**Alerts** go here\n\nand more below".into();
    store.apply_channels(vec![with_header]);

    assert_eq!(
        store.channel_heading(&ChannelId::new("c1")).topic,
        "Alerts go here"
    );
}

/// A channel with no header, or one that is only blank lines, says nothing
/// rather than showing an empty separator beside its name.
#[test]
fn a_channel_with_no_header_has_no_topic() {
    let mut store = store();
    let mut blank = channel("c2", 0, 0);
    blank.header = "\n   \n".into();
    store.apply_channels(vec![blank, channel("c3", 0, 0)]);

    assert!(
        store
            .channel_heading(&ChannelId::new("c2"))
            .topic
            .is_empty()
    );
    assert!(
        store
            .channel_heading(&ChannelId::new("c3"))
            .topic
            .is_empty()
    );
}

/// The one-click reactions are the ones this reader actually uses. A
/// client that hardcodes three offers two people in three the wrong ones.
#[test]
fn quick_reactions_come_from_what_the_reader_uses() {
    let mut store = store();
    for (post, emoji) in [
        ("p1", "tada"),
        ("p2", "tada"),
        ("p3", "tada"),
        ("p4", "rocket"),
        ("p5", "rocket"),
    ] {
        store.apply_ws(&WsEvent::ReactionAdded(reaction(post, ME, emoji, 10)));
    }
    // Somebody else's reactions are not this reader's habits.
    store.apply_ws(&WsEvent::ReactionAdded(reaction(
        "p6", "u-other", "zzz", 10,
    )));
    store.apply_ws(&WsEvent::ReactionAdded(reaction(
        "p7", "u-other", "zzz", 10,
    )));

    assert_eq!(store.favourite_emoji(2), ["tada", "rocket"]);
}

/// A new account has reacted to nothing, and must still get three buttons
/// rather than none.
#[test]
fn quick_reactions_fall_back_to_a_common_set() {
    let store = store();
    assert_eq!(store.favourite_emoji(3).len(), 3);

    // And a reader with one habit gets it first, padded with the rest.
    let mut one = self::store();
    one.apply_ws(&WsEvent::ReactionAdded(reaction("p1", ME, "tada", 10)));
    let offered = one.favourite_emoji(3);
    assert_eq!(offered[0], "tada");
    assert_eq!(offered.len(), 3);
    assert!(!offered[1..].contains(&"tada".to_owned()), "{offered:?}");
}

/// A link to a post is `{server}/{team}/pl/{post}`, which is what pasting
/// one into Mattermost resolves.
#[test]
fn a_post_has_a_permalink() {
    let mut store = store();
    store.apply_teams(vec![Team {
        id: TeamId::new("t1"),
        name: "alanbase".into(),
        ..Team::default()
    }]);
    let mut channel = channel("c1", 0, 0);
    channel.team_id = TeamId::new("t1");
    store.apply_channels(vec![channel]);

    let mut page = PostList::default();
    let only = post("p1", "c1", 10);
    page.order.push(only.id.clone());
    page.posts.insert(only.id.clone(), only);
    store.apply_post_list(&ChannelId::new("c1"), &page, PageAnchor::Newest);

    assert_eq!(
        store.permalink("https://matter.example.invalid/", &PostId::new("p1")),
        Some("https://matter.example.invalid/alanbase/pl/p1".to_owned())
    );
    // A post nobody has loaded has no link.
    assert_eq!(
        store.permalink("https://x.invalid", &PostId::new("p-none")),
        None
    );
}

/// A reader with more habits than there are buttons gets the commonest,
/// and one whose habit is already in the default set is not offered it
/// twice.
#[test]
fn quick_reactions_are_capped_and_never_repeat() {
    let mut store = store();
    for (post, emoji, times) in [
        ("p1", "tada", 4),
        ("p2", "rocket", 3),
        ("p3", "eyes", 2),
        ("p4", "heart", 1),
    ] {
        for n in 0..times {
            // A distinct post each time: one person reacts to one post
            // with one emoji once.
            let id = format!("{post}-{n}");
            store.apply_ws(&WsEvent::ReactionAdded(reaction(&id, ME, emoji, 10)));
        }
    }
    assert_eq!(store.favourite_emoji(3), ["tada", "rocket", "eyes"]);

    // A reader whose only habit is already one of the defaults is not
    // offered it twice: the padding skips what is already there.
    let mut overlapping = self::store();
    overlapping.apply_ws(&WsEvent::ReactionAdded(reaction("p9", ME, "eyes", 10)));
    let offered = overlapping.favourite_emoji(3);
    assert_eq!(offered[0], "eyes");
    assert_eq!(
        offered.iter().filter(|e| *e == "eyes").count(),
        1,
        "{offered:?}"
    );
    assert_eq!(offered.len(), 3);
}
