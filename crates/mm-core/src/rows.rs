//! The post-list projection: date separators and author grouping.
//!
//! This is boundary logic — "did the day change?", "is this the same person
//! still talking?" — and boundary logic is where chat clients are wrong in
//! ways nobody notices for months. So it lives here, at 100%, rather than
//! inside a GTK callback where it could only be checked by looking at it.
//!
//! **No clock is read.** A calendar day depends on the reader's timezone, so
//! the offset arrives as a parameter. The caller holds the clock; this
//! function is a total function of its arguments, and the pre-epoch and
//! across-midnight cases are therefore testable without a fake.

use mm_types::{Post, PostId, Timestamp, UserId};

/// Two posts by the same author closer together than this are drawn as one
/// block. Matches the webapp's `POST_COLLAPSE_TIMEOUT`.
const GROUP_WINDOW_MILLIS: i64 = 5 * 60 * 1000;

/// A date in the proleptic Gregorian calendar, in the reader's timezone.
///
/// Kept as a plain civil date rather than a timestamp because the question
/// being asked is "is this a different day?", and that question has a wrong
/// answer in milliseconds: two posts 40 seconds apart can be on different
/// days, and two posts 23 hours apart need not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilDate {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

/// One line of the post list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// Where the reader left off. Drawn once, above the first message they
    /// have not seen.
    NewMessages,
    /// A day boundary. Carries both the civil date and the timestamp that
    /// produced it: the date is what the caller compares against "today", the
    /// timestamp is what it formats for display.
    Date { at: Timestamp, date: CivilDate },
    /// A message. `grouped` means the author line is redundant — the same
    /// person posted a moment ago, on the same day.
    Post { id: PostId, grouped: bool },
}

/// Project a channel's posts, in ascending order, into display rows.
///
/// `utc_offset_secs` is the reader's offset from UTC, east positive.
/// `unread_after` is the reader's last-viewed time *as it was when they
/// opened the channel*: opening marks it read, so taking it live would make
/// the line vanish in the instant it became useful.
#[must_use]
pub fn project(
    posts: &[&Post],
    utc_offset_secs: i32,
    unread_after: Option<Timestamp>,
    me: &UserId,
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(posts.len() + 2);
    let mut previous: Option<(&Post, CivilDate)> = None;
    let mut marked = false;

    for post in posts {
        // Where the reader left off, by time alone. This used to be kept
        // off the very first row on the grounds that "new" above
        // everything says nothing -- but `posts` is a window onto the
        // channel, not the channel, and a line that is forbidden from
        // the top of the window lands under the first message of
        // whatever the window happens to show, and moves every time the
        // window does. A line at the top is honest: everything below it
        // is new, and scrolling back finds the message it belongs
        // above.
        //
        // A reader who has never read the channel at all has no line:
        // nothing was ever seen, so there is nothing to separate the new
        // from.
        //
        // And never above the reader's own message: they were there when
        // they wrote it, whatever the server's view time says, and a line
        // saying "new" over their own words is the webapp's rule broken.
        let starts_unread = !marked
            && post.user_id != *me
            && unread_after.is_some_and(|at| !at.is_zero() && post.create_at > at);

        let date = civil_date(post.create_at, utc_offset_secs);
        let same_day = previous.is_some_and(|(_, d)| d == date);
        if !same_day {
            rows.push(Row::Date {
                at: post.create_at,
                date,
            });
        }
        // Under the date, immediately above the message it refers to.
        if starts_unread {
            rows.push(Row::NewMessages);
            marked = true;
        }
        rows.push(Row::Post {
            id: post.id.clone(),
            // A message directly under the line keeps its author line: the
            // first thing the reader sees on returning should say who is
            // talking, even if it is the same person as before.
            grouped: same_day
                && !starts_unread
                && previous.is_some_and(|(p, _)| continues(p, post)),
        });
        previous = Some((post, date));
    }
    rows
}

/// Does `post` continue `previous`'s block?
///
/// A system message ("Alice joined the channel") never groups in either
/// direction: it is not the author talking, and hiding the author line would
/// attribute it to whoever spoke last.
fn continues(previous: &Post, post: &Post) -> bool {
    previous.user_id == post.user_id
        && !previous.is_system()
        && !post.is_system()
        // A webhook is never a continuation, in either direction. One bot
        // account posts under many names -- "Alertmanager", "Deploys", "CI"
        // -- so sharing a `user_id` says nothing about sharing an author,
        // and an alert channel collapsed into one block of a dozen alerts
        // with a single heading, which is not what any other client shows.
        && !previous.is_from_webhook()
        && !post.is_from_webhook()
        // Replies to different roots are different conversations even when
        // the same person wrote both. Invisible with collapsed threads,
        // where replies are not in the channel at all -- and wrong the
        // moment somebody turns that off.
        && previous.root_id == post.root_id
        && post.create_at.millis() - previous.create_at.millis() <= GROUP_WINDOW_MILLIS
}

/// The civil date a timestamp falls on, at a given offset from UTC.
#[must_use]
pub fn civil_date(at: Timestamp, utc_offset_secs: i32) -> CivilDate {
    let local_secs = at.millis().div_euclid(1000) + i64::from(utc_offset_secs);
    civil_from_days(local_secs.div_euclid(86_400))
}

/// Howard Hinnant's `civil_from_days`, which is exact for every `i64` day
/// number rather than only for dates a `tm` struct can hold.
///
/// Written out rather than pulled from `chrono` on purpose: `mm-core` may not
/// depend on anything that reads a clock, and this is the whole of what is
/// needed. The shift by 719468 moves the epoch to 0000-03-01, which puts the
/// leap day at the end of the year and makes the month arithmetic branchless.
fn civil_from_days(days: i64) -> CivilDate {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    CivilDate {
        year: yoe + era * 400 + i64::from(month <= 2),
        // Both are bounded by the arithmetic above; the fallbacks are
        // unreachable and exist only to keep the conversion total.
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

#[cfg(test)]
#[path = "tests/rows.rs"]
mod tests;
