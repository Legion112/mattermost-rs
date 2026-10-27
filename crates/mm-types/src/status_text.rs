//! The emoji and words somebody puts beside their name.
//!
//! Mattermost calls this a *custom status*, and it is **a JSON
//! document inside a JSON string inside `props`** -- the same wart as
//! `post` and `reaction` on the WebSocket, in a different place. It
//! also **expires**, and most of the ones on a real server have:
//! people set "Out for lunch" for an hour and never clear it, so a
//! client that ignores `expires_at` shows last October's lunch.

use serde::Deserialize;

/// An instant as the server writes it, to the second.
///
/// Enough to compare two of them, which is all an expiry needs, and
/// it keeps this crate free of a date library and of a clock -- the
/// caller says what "now" is, the way everything else here does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcMoment {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl UtcMoment {
    /// Read `2026-10-27T12:40:00Z`, with or without fractional
    /// seconds, and with or without the `Z`.
    ///
    /// Only the shape Mattermost writes. An offset other than UTC
    /// would need real arithmetic, and the server does not send one.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let digits =
            |at: std::ops::Range<usize>| -> Option<u32> { raw.get(at)?.parse::<u32>().ok() };
        if raw.len() < 19 || raw.as_bytes().get(10) != Some(&b'T') {
            return None;
        }
        Some(Self {
            year: raw.get(0..4)?.parse().ok()?,
            month: digits(5..7)?,
            day: digits(8..10)?,
            hour: digits(11..13)?,
            minute: digits(14..16)?,
            second: digits(17..19)?,
        })
    }
}

/// What somebody is up to, as they wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CustomStatus {
    /// A shortcode without colons. Can be one of the server's own
    /// custom emoji, so it is drawn through the same path as a
    /// message's.
    pub emoji: String,
    pub text: String,
    /// The wire form, kept as the server wrote it.
    pub expires_at: String,
}

impl CustomStatus {
    /// Read the JSON document the `customStatus` prop holds.
    ///
    /// `None` for anything that says nothing: no prop, unparseable,
    /// or an emoji and text that are both empty -- which is what is
    /// left behind when somebody clears their status.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let status: Self = serde_json::from_str(raw).ok()?;
        (!status.emoji.is_empty() || !status.text.trim().is_empty()).then_some(status)
    }

    /// Whether it has run out.
    ///
    /// A status with no expiry never does, and the server writes that
    /// as the year-one sentinel `0001-01-01T00:00:00Z` rather than as
    /// an absent field. Anything that cannot be read is treated as
    /// not expiring, because hiding what somebody wrote is worse than
    /// showing it a little too long.
    #[must_use]
    pub fn is_expired(&self, now: UtcMoment) -> bool {
        UtcMoment::parse(&self.expires_at).is_some_and(|at| at.year > 1 && at <= now)
    }
}

#[cfg(test)]
#[path = "tests/status_text.rs"]
mod tests;
