use super::*;
use mm_types::UserId;

const MINUTE: i64 = 60 * 1000;
const DAY: i64 = 24 * 60 * MINUTE;

/// 2021-01-01T00:00:00Z, chosen because it is a Friday and not a leap year,
/// so an off-by-one in either the weekday or the leap arithmetic moves it.
const NEW_YEAR_2021: i64 = 1_609_459_200_000;

fn post(id: &str, author: &str, created: i64) -> Post {
    Post {
        id: PostId::new(id),
        user_id: UserId::new(author),
        create_at: Timestamp(created),
        update_at: Timestamp(created),
        message: format!("message {id}"),
        ..Post::default()
    }
}

fn system(id: &str, author: &str, created: i64) -> Post {
    Post {
        kind: "system_join_channel".into(),
        ..post(id, author, created)
    }
}

/// The reader, who wrote none of the posts these tests make unless a
/// test says so.
const ME: &str = "me";

/// `super::project` as the reader `ME`, which is what nearly every test
/// here wants and what keeps them to the facts they are about.
fn project(posts: &[&Post], utc_offset_secs: i32, unread_after: Option<Timestamp>) -> Vec<Row> {
    super::project(posts, utc_offset_secs, unread_after, &UserId::new(ME))
}

fn ids(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            Row::Date { date, .. } => {
                format!("--- {}-{:02}-{:02}", date.year, date.month, date.day)
            }
            Row::NewMessages => "--- new messages".to_owned(),
            Row::Post { id, grouped } => {
                format!(
                    "{}{}",
                    id.as_str(),
                    if *grouped { " (grouped)" } else { "" }
                )
            }
        })
        .collect()
}

/// An empty channel renders as nothing at all, not as a stray separator for
/// a day on which nobody spoke.
#[test]
fn no_posts_means_no_rows() {
    assert!(project(&[], 0, None).is_empty());
}

/// The first post of a channel always gets a separator above it: there is no
/// previous day to compare against, and starting mid-air reads as a list
/// that was cut off.
#[test]
fn the_first_post_opens_a_day() {
    let only = post("p1", "alice", NEW_YEAR_2021);
    assert_eq!(ids(&project(&[&only], 0, None)), ["--- 2021-01-01", "p1"]);
}

/// The whole point of grouping: one person saying three things in a row is
/// one block with one author line.
#[test]
fn the_same_author_in_quick_succession_groups() {
    let a = post("p1", "alice", NEW_YEAR_2021);
    let b = post("p2", "alice", NEW_YEAR_2021 + MINUTE);
    let c = post("p3", "alice", NEW_YEAR_2021 + 2 * MINUTE);
    assert_eq!(
        ids(&project(&[&a, &b, &c], 0, None)),
        ["--- 2021-01-01", "p1", "p2 (grouped)", "p3 (grouped)"]
    );
}

/// Five minutes is the boundary, and it is inclusive. A reply arriving six
/// minutes later is a new thought and gets its own author line and time.
#[test]
fn a_gap_longer_than_five_minutes_breaks_the_block() {
    let a = post("p1", "alice", NEW_YEAR_2021);
    let exactly = post("p2", "alice", NEW_YEAR_2021 + 5 * MINUTE);
    let over = post("p3", "alice", NEW_YEAR_2021 + 11 * MINUTE);
    assert_eq!(
        ids(&project(&[&a, &exactly, &over], 0, None)),
        ["--- 2021-01-01", "p1", "p2 (grouped)", "p3"]
    );
}

/// Grouping is per author. Interleaved speakers must never collapse, or a
/// message is attributed to the wrong person -- the worst bug this projection
/// can have.
#[test]
fn a_different_author_breaks_the_block() {
    let a = post("p1", "alice", NEW_YEAR_2021);
    let b = post("p2", "bob", NEW_YEAR_2021 + MINUTE);
    let c = post("p3", "alice", NEW_YEAR_2021 + 2 * MINUTE);
    assert_eq!(
        ids(&project(&[&a, &b, &c], 0, None)),
        ["--- 2021-01-01", "p1", "p2", "p3"]
    );
}

/// A system message is about the author, not by them. Grouping it would hide
/// its author line and read as the previous speaker saying it; grouping the
/// message after it would attribute that message to the system line.
#[test]
fn a_system_message_groups_in_neither_direction() {
    let a = post("p1", "alice", NEW_YEAR_2021);
    let joined = system("p2", "alice", NEW_YEAR_2021 + MINUTE);
    let b = post("p3", "alice", NEW_YEAR_2021 + 2 * MINUTE);
    assert_eq!(
        ids(&project(&[&a, &joined, &b], 0, None)),
        ["--- 2021-01-01", "p1", "p2", "p3"]
    );
}

/// Midnight ends a block even when the two posts are seconds apart. Without
/// this a conversation running past midnight shows a separator with a grouped
/// post directly under it and no author line at all.
#[test]
fn a_day_boundary_breaks_the_block_however_close_the_posts_are() {
    let before = post("p1", "alice", NEW_YEAR_2021 - 1000);
    let after = post("p2", "alice", NEW_YEAR_2021);
    assert_eq!(
        ids(&project(&[&before, &after], 0, None)),
        ["--- 2020-12-31", "p1", "--- 2021-01-01", "p2"]
    );
}

/// The reader's timezone decides the day, not UTC. At UTC+5 the two posts
/// above are both on New Year's Day and group normally -- and at UTC-5 a post
/// made at 02:00 UTC belongs to the previous evening.
#[test]
fn the_offset_decides_which_day_a_post_falls_on() {
    let evening = post("p1", "alice", NEW_YEAR_2021 - 1000);
    let midnight = post("p2", "alice", NEW_YEAR_2021);
    assert_eq!(
        ids(&project(&[&evening, &midnight], 5 * 3600, None)),
        ["--- 2021-01-01", "p1", "p2 (grouped)"]
    );

    let small_hours = post("p3", "alice", NEW_YEAR_2021 + 2 * 3600 * 1000);
    assert_eq!(
        ids(&project(&[&small_hours], -5 * 3600, None)),
        ["--- 2020-12-31", "p3"]
    );
}

/// The separator carries the timestamp that produced it, because the caller
/// formats the date with its own locale rules and cannot recover a timestamp
/// from a civil date without redoing this arithmetic backwards.
#[test]
fn a_separator_carries_the_timestamp_of_the_day_it_opens() {
    let first = post("p1", "alice", NEW_YEAR_2021 + 9 * 3600 * 1000);
    let Some(Row::Date { at, date }) = project(&[&first], 0, None).first().cloned() else {
        panic!("expected a date row first");
    };
    assert_eq!(at, Timestamp(NEW_YEAR_2021 + 9 * 3600 * 1000));
    assert_eq!(
        date,
        CivilDate {
            year: 2021,
            month: 1,
            day: 1
        }
    );
}

/// The calendar arithmetic, at the points where an off-by-one hides: the
/// epoch itself, the day before it (negative days, which a naive integer
/// division gets wrong), both ends of a leap year, the century rule that
/// makes 1900 not a leap year, and the 400-year rule that makes 2000 one.
#[test]
fn the_calendar_is_right_at_every_awkward_boundary() {
    let cases = [
        (0, 1970, 1, 1),
        (-1, 1969, 12, 31),
        (-86_400_000, 1969, 12, 31),
        (951_782_400_000, 2000, 2, 29),
        (951_868_800_000, 2000, 3, 1),
        (-2_203_977_600_000, 1900, 2, 28),
        (-2_203_891_200_000, 1900, 3, 1),
        (1_640_908_800_000, 2021, 12, 31),
        (4_102_444_800_000, 2100, 1, 1),
    ];
    for (millis, year, month, day) in cases {
        assert_eq!(
            civil_date(Timestamp(millis), 0),
            CivilDate { year, month, day },
            "for {millis}"
        );
    }
}

/// Every day in a four-year window round-trips to a distinct, ordered date.
/// This is the cheap way to prove the month arithmetic has no gaps or
/// duplicates without enumerating 1461 expected values by hand.
#[test]
fn consecutive_days_produce_consecutive_distinct_dates() {
    let mut seen = Vec::new();
    for day in 0..1461 {
        seen.push(civil_date(Timestamp(NEW_YEAR_2021 + day * DAY), 0));
    }
    let mut sorted = seen.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "a date repeated");
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "dates went backwards");
    assert_eq!(
        seen.last(),
        Some(&CivilDate {
            year: 2024,
            month: 12,
            day: 31
        })
    );
}

/// The line goes above the first message the reader had not seen, and the
/// message under it keeps its author line even when the same person was
/// talking before -- the first thing you see on returning should say who is
/// speaking.
#[test]
fn the_new_messages_line_marks_where_the_reader_left_off() {
    let seen = post("p1", "alice", NEW_YEAR_2021);
    let first_new = post("p2", "alice", NEW_YEAR_2021 + MINUTE);
    let also_new = post("p3", "alice", NEW_YEAR_2021 + 2 * MINUTE);
    assert_eq!(
        ids(&project(
            &[&seen, &first_new, &also_new],
            0,
            Some(Timestamp(NEW_YEAR_2021))
        )),
        [
            "--- 2021-01-01",
            "p1",
            "--- new messages",
            "p2",
            "p3 (grouped)"
        ]
    );
}

/// It is drawn once, not above every unread message.
#[test]
fn the_line_is_drawn_once() {
    let seen = post("p1", "alice", NEW_YEAR_2021);
    let a = post("p2", "bob", NEW_YEAR_2021 + MINUTE);
    let b = post("p3", "bob", NEW_YEAR_2021 + 20 * MINUTE);
    let rows = project(&[&seen, &a, &b], 0, Some(Timestamp(NEW_YEAR_2021)));
    assert_eq!(rows.iter().filter(|r| **r == Row::NewMessages).count(), 1);
}

/// A reader with nothing unread, and a reader who has read nothing at all,
/// both get no line: above everything it would say nothing the reader
/// cannot already see.
#[test]
fn no_line_when_there_is_nothing_above_it_to_separate() {
    let a = post("p1", "alice", NEW_YEAR_2021);
    let b = post("p2", "alice", NEW_YEAR_2021 + MINUTE);

    // Everything already seen.
    let caught_up = project(&[&a, &b], 0, Some(Timestamp(NEW_YEAR_2021 + 2 * MINUTE)));
    assert!(!caught_up.contains(&Row::NewMessages));

    // Never read: nothing was ever seen, so there is nothing to separate
    // the new from, and a line would only say "all of it".
    let brand_new = project(&[&a, &b], 0, Some(Timestamp::ZERO));
    assert!(!brand_new.contains(&Row::NewMessages));

    // But a window that starts inside the unread -- the reader opened a
    // channel with more new messages than fit -- gets the line at its
    // top. By time, not by position: a line kept off the first row would
    // land under the first message of whatever is shown, and move every
    // time the window did.
    let from_inside = project(&[&a, &b], 0, Some(Timestamp(NEW_YEAR_2021 - MINUTE)));
    assert_eq!(
        ids(&from_inside),
        ["--- 2021-01-01", "--- new messages", "p1", "p2 (grouped)"]
    );

    // And no last-viewed time at all means no line.
    assert!(!project(&[&a, &b], 0, None).contains(&Row::NewMessages));
}

/// The line sits under the date separator, not above it: the date is a
/// property of the messages below, and the line points at one of them.
#[test]
fn the_line_sits_under_the_date_it_falls_on() {
    let yesterday = post("p1", "alice", NEW_YEAR_2021 - 1000);
    let today = post("p2", "alice", NEW_YEAR_2021);
    assert_eq!(
        ids(&project(
            &[&yesterday, &today],
            0,
            Some(Timestamp(NEW_YEAR_2021 - 500))
        )),
        [
            "--- 2020-12-31",
            "p1",
            "--- 2021-01-01",
            "--- new messages",
            "p2"
        ]
    );
}

fn from_webhook(id: &str, author: &str, created: i64) -> Post {
    Post {
        props: serde_json::json!({"from_webhook": "true", "override_username": "Alertmanager"}),
        ..post(id, author, created)
    }
}

/// One bot account posts under many names, so two posts sharing a `user_id`
/// say nothing about sharing an author. Grouping them collapsed a channel
/// of alerts into a single block with one heading -- which is why an alert
/// channel looked nothing like it does in every other client.
#[test]
fn webhook_posts_never_group() {
    let a = from_webhook("p1", "u-bot", NEW_YEAR_2021);
    let b = from_webhook("p2", "u-bot", NEW_YEAR_2021 + MINUTE);
    assert_eq!(
        ids(&project(&[&a, &b], 0, None)),
        ["--- 2021-01-01", "p1", "p2"]
    );
}

/// In either direction: a person's message after an alert is not a
/// continuation of the alert, and an alert after their message is not a
/// continuation of them.
#[test]
fn a_webhook_breaks_the_block_on_both_sides() {
    let spoke = post("p1", "u-bot", NEW_YEAR_2021);
    let alert = from_webhook("p2", "u-bot", NEW_YEAR_2021 + MINUTE);
    let spoke_again = post("p3", "u-bot", NEW_YEAR_2021 + 2 * MINUTE);
    assert_eq!(
        ids(&project(&[&spoke, &alert, &spoke_again], 0, None)),
        ["--- 2021-01-01", "p1", "p2", "p3"]
    );
}

/// Two replies to different roots are two conversations, even from one
/// person a minute apart. Invisible while collapsed threads hide replies
/// from the channel, and wrong the moment somebody turns that off.
#[test]
fn replies_to_different_roots_do_not_group() {
    let mut first = post("p1", "alice", NEW_YEAR_2021);
    first.root_id = PostId::new("root-a");
    let mut second = post("p2", "alice", NEW_YEAR_2021 + MINUTE);
    second.root_id = PostId::new("root-b");
    assert_eq!(
        ids(&project(&[&first, &second], 0, None)),
        ["--- 2021-01-01", "p1", "p2"]
    );

    // Two replies to the *same* root still group.
    let mut third = post("p3", "alice", NEW_YEAR_2021 + 2 * MINUTE);
    third.root_id = PostId::new("root-b");
    assert_eq!(
        ids(&project(&[&second, &third], 0, None)),
        ["--- 2021-01-01", "p2", "p3 (grouped)"]
    );
}

/// The line never sits above the reader's own message: they were there
/// when they wrote it, whatever the server's view time says. It goes
/// above the next message from somebody else, if there is one.
#[test]
fn no_line_above_the_readers_own_message() {
    let seen = post("p1", "alice", NEW_YEAR_2021);
    let mine = post("p2", ME, NEW_YEAR_2021 + MINUTE);
    let theirs = post("p3", "bob", NEW_YEAR_2021 + 2 * MINUTE);
    let rows = project(&[&seen, &mine, &theirs], 0, Some(Timestamp(NEW_YEAR_2021)));
    assert_eq!(
        ids(&rows),
        ["--- 2021-01-01", "p1", "p2", "--- new messages", "p3"],
        "{rows:?}"
    );
    // With nothing of anybody else's after it, no line at all.
    let rows = project(&[&seen, &mine], 0, Some(Timestamp(NEW_YEAR_2021)));
    assert!(!rows.contains(&Row::NewMessages), "{rows:?}");
}
