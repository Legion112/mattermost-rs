use super::*;
use mm_types::ChannelId;
use std::collections::HashMap;

fn post(id: &str, created: i64) -> Post {
    Post {
        id: PostId::new(id),
        create_at: Timestamp(created),
        channel_id: ChannelId::new("c1"),
        ..Post::default()
    }
}

/// Build a page whose `order` is newest-first, as the server sends it.
/// `prev` is the `prev_post_id` cursor: empty means "nothing older exists".
fn page_with_prev(posts: Vec<Post>, prev: &str) -> PostList {
    let mut map = HashMap::new();
    let mut order: Vec<PostId> = posts.iter().map(|p| p.id.clone()).collect();
    order.reverse();
    for p in posts {
        map.insert(p.id.clone(), p);
    }
    PostList {
        order,
        posts: map,
        prev_post_id: prev.to_owned(),
        ..PostList::default()
    }
}

fn page(posts: Vec<Post>) -> PostList {
    page_with_prev(posts, "")
}

#[test]
fn empty_timeline_reports_itself_empty() {
    let t = Timeline::default();
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert_eq!(t.newest(), None);
    assert_eq!(t.oldest(), None);
    assert!(!t.older_complete());
}

#[test]
fn insert_keeps_posts_in_ascending_order() {
    let mut t = Timeline::default();
    for (id, at) in [("p3", 30), ("p1", 10), ("p2", 20)] {
        assert!(t.insert(&post(id, at)));
    }
    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["p1", "p2", "p3"]);
    assert_eq!(t.oldest().unwrap().id.as_str(), "p1");
    assert_eq!(t.newest().unwrap().id.as_str(), "p3");
}

/// The server replays its dead queue after a reconnect, so the same `posted`
/// event genuinely arrives twice. A duplicate must be a no-op, not a second
/// copy of the message on screen.
#[test]
fn inserting_the_same_post_twice_is_a_no_op() {
    let mut t = Timeline::default();
    assert!(t.insert(&post("p1", 10)));
    assert!(!t.insert(&post("p1", 10)));
    assert_eq!(t.len(), 1);
}

/// `create_at` is milliseconds and collisions happen in bulk imports and
/// webhook bursts. Without the id as tie-breaker the order is not total and
/// the merge is non-deterministic.
#[test]
fn posts_sharing_a_millisecond_are_ordered_by_id() {
    let mut t = Timeline::default();
    t.insert(&post("pb", 10));
    t.insert(&post("pa", 10));
    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["pa", "pb"]);
    assert_eq!(t.len(), 2, "same timestamp must not collapse two posts");
}

#[test]
fn remove_drops_a_held_post_and_ignores_an_absent_one() {
    let mut t = Timeline::default();
    t.insert(&post("p1", 10));
    assert!(t.remove(&post("p1", 10)));
    assert!(t.is_empty());
    assert!(!t.remove(&post("p1", 10)));
}

#[test]
fn contains_finds_what_was_inserted() {
    let mut t = Timeline::default();
    let p = post("p1", 10);
    assert!(!t.contains(&PostKey::of(&p)));
    t.insert(&p);
    assert!(t.contains(&PostKey::of(&p)));
}

#[test]
fn merging_into_an_empty_timeline_takes_the_whole_page() {
    let mut t = Timeline::default();
    let merged = t.merge_page(
        &page(vec![post("p1", 10), post("p2", 20), post("p3", 30)]),
        PageAnchor::Newest,
    );
    assert_eq!(merged, Merge::Added(3));
    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["p1", "p2", "p3"]);
}

#[test]
fn merging_an_empty_page_changes_nothing() {
    let mut t = Timeline::default();
    assert_eq!(
        t.merge_page(&PostList::default(), PageAnchor::Newest),
        Merge::NoChange
    );
}

#[test]
fn re_merging_the_same_page_changes_nothing() {
    let mut t = Timeline::default();
    let p = page(vec![post("p1", 10), post("p2", 20)]);
    assert_eq!(t.merge_page(&p, PageAnchor::Newest), Merge::Added(2));
    assert_eq!(t.merge_page(&p, PageAnchor::Newest), Merge::NoChange);
    assert_eq!(t.len(), 2);
}

/// An overlapping page is adjacent by evidence, whatever the anchor claims.
#[test]
fn an_overlapping_page_merges_even_when_unanchored() {
    let mut t = Timeline::default();
    t.merge_page(
        &page(vec![post("p2", 20), post("p3", 30)]),
        PageAnchor::Newest,
    );
    let merged = t.merge_page(
        &page(vec![post("p1", 10), post("p2", 20)]),
        PageAnchor::Unanchored,
    );
    assert_eq!(merged, Merge::Added(1));
    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["p1", "p2", "p3"]);
}

/// Paging backwards shares no post with what is held — `before=p5` returns
/// posts strictly older than p5 — yet it is adjacent by construction. This is
/// why the anchor is the caller's knowledge and not something read off the
/// page: an earlier version tried to infer it from the cursors and was wrong.
#[test]
fn paging_backwards_merges_although_it_shares_no_post() {
    let mut t = Timeline::default();
    t.merge_page(&page(vec![post("p5", 50)]), PageAnchor::Newest);

    let older = page(vec![post("p3", 30), post("p4", 40)]);
    assert_eq!(
        t.merge_page(&older, PageAnchor::Unanchored),
        Merge::WouldLeaveGap,
        "with no claim of adjacency, a disjoint page must be refused"
    );
    assert_eq!(
        t.merge_page(&older, PageAnchor::OlderThanHeld),
        Merge::Added(2),
        "but the same page is adjacent when it was fetched with before="
    );
    assert_eq!(t.len(), 3);
}

#[test]
fn paging_forwards_merges_although_it_shares_no_post() {
    let mut t = Timeline::default();
    t.merge_page(&page(vec![post("p1", 10)]), PageAnchor::Newest);
    let newer = page(vec![post("p2", 20)]);
    assert_eq!(
        t.merge_page(&newer, PageAnchor::NewerThanHeld),
        Merge::Added(1)
    );
}

/// The invariant worth having. A page whose adjacency cannot be established
/// would, if merged, put two disjoint runs next to each other, and the user
/// would scroll straight past the messages in between with nothing to say so.
#[test]
fn a_disjoint_page_is_refused_rather_than_inventing_adjacency() {
    let mut t = Timeline::default();
    t.merge_page(&page(vec![post("p1", 10)]), PageAnchor::Newest);

    let far_future = page(vec![post("p9", 900)]);
    assert_eq!(
        t.merge_page(&far_future, PageAnchor::Unanchored),
        Merge::WouldLeaveGap
    );
    assert_eq!(t.len(), 1, "nothing may be added when a gap is refused");

    // A re-fetched newest page that does not reach back to what is held is the
    // same situation: it arrives after a long disconnect.
    assert_eq!(
        t.merge_page(&far_future, PageAnchor::Newest),
        Merge::WouldLeaveGap
    );
    assert_eq!(t.len(), 1);
}

/// When the gap is too large to close, the rule is to keep the fresh tail and
/// drop the stale run rather than page backwards indefinitely.
#[test]
fn resetting_to_a_page_discards_the_stale_run() {
    let mut t = Timeline::default();
    t.merge_page(&page(vec![post("p1", 10)]), PageAnchor::Newest);
    t.set_older_complete(true);

    let fresh = page(vec![post("p8", 800), post("p9", 900)]);
    assert_eq!(t.reset_to_page(&fresh), Merge::Added(2));

    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["p8", "p9"]);
    assert!(
        !t.older_complete(),
        "the start of the channel is no longer known to be held"
    );
}

/// Deleted posts arrive from `?since=`, which has no `DeleteAt = 0` filter.
#[test]
fn deleted_posts_in_a_page_are_not_added() {
    let mut t = Timeline::default();
    let mut gone = post("p2", 20);
    gone.delete_at = Timestamp(25);
    let merged = t.merge_page(&page(vec![post("p1", 10), gone]), PageAnchor::Newest);
    assert_eq!(merged, Merge::Added(1));
    let ids: Vec<&str> = t.keys().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, vec!["p1"]);
}

/// A page listing an id it did not include must be skipped, not panic.
#[test]
fn a_page_whose_order_names_a_missing_post_is_tolerated() {
    let mut t = Timeline::default();
    let mut p = page(vec![post("p1", 10)]);
    p.order.push(PostId::new("absent"));
    assert_eq!(t.merge_page(&p, PageAnchor::Newest), Merge::Added(1));
}

#[test]
fn older_complete_is_settable_and_reported() {
    let mut t = Timeline::default();
    assert!(!t.older_complete());
    t.set_older_complete(true);
    assert!(t.older_complete());
}

#[test]
fn a_page_cursor_is_still_available_to_the_caller() {
    // The timeline does not read the cursors, but the store does, to decide
    // whether the start of the channel has been reached.
    let p = page_with_prev(vec![post("p1", 10)], "p0");
    assert_eq!(p.prev_post_id, "p0");
    assert!(page(vec![post("p1", 10)]).prev_post_id.is_empty());
}

#[test]
fn every_derive_is_exercised() {
    let mut t = Timeline::default();
    t.insert(&post("p1", 10));
    assert_eq!(t, t.clone());
    assert!(!format!("{t:?}").is_empty());

    let k = PostKey::of(&post("p1", 10));
    assert_eq!(k, k.clone());
    assert!(!format!("{k:?}").is_empty());
    assert!(k < PostKey::of(&post("p2", 20)));

    let m = Merge::NoChange;
    assert_eq!(m, m.clone());
    assert!(!format!("{m:?}").is_empty());
    assert_ne!(Merge::Added(1), Merge::WouldLeaveGap);

    let a = PageAnchor::Newest;
    assert_eq!(a, a.clone());
    assert!(!format!("{a:?}").is_empty());
    assert_ne!(PageAnchor::OlderThanHeld, PageAnchor::NewerThanHeld);
    assert_ne!(PageAnchor::Unanchored, PageAnchor::Newest);
}
