use super::*;
use mm_types::{ChannelType, PostList};
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
