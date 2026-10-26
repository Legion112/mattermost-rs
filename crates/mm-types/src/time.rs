//! Timestamps.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Milliseconds since the Unix epoch, which is what every Mattermost
/// timestamp field is.
///
/// Zero is not "unset" in general — but for `delete_at`, `edit_at` and
/// `last_post_at` the server does use `0` to mean "never", so `is_zero` is a
/// meaningful question rather than a null check.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// For `delete_at` / `edit_at` / `last_post_at`, zero means "never".
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Later of the two. Used throughout `mm-core`, where timestamps only ever
    /// move forward: a read marker that could go backwards would make unread
    /// state flap between devices.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        if other.0 > self.0 { other } else { self }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i64> for Timestamp {
    fn from(millis: i64) -> Self {
        Self(millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_a_bare_number() {
        assert_eq!(serde_json::to_string(&Timestamp(17)).unwrap(), "17");
        let back: Timestamp = serde_json::from_str("17").unwrap();
        assert_eq!(back, Timestamp(17));
    }

    /// The server writes 0, not null, for "never deleted" and "never edited".
    #[test]
    fn zero_means_never() {
        assert!(Timestamp::ZERO.is_zero());
        assert!(Timestamp::default().is_zero());
        assert!(!Timestamp(1).is_zero());
    }

    /// `max` is how every read marker in mm-core is updated. If it ever went
    /// backwards, a stale `channel_viewed` from another device would re-unread
    /// a channel the user is looking at.
    #[test]
    fn max_never_goes_backwards() {
        assert_eq!(Timestamp(5).max(Timestamp(9)), Timestamp(9));
        assert_eq!(Timestamp(9).max(Timestamp(5)), Timestamp(9));
        assert_eq!(Timestamp(5).max(Timestamp(5)), Timestamp(5));
    }

    #[test]
    fn every_derive_and_conversion_is_exercised() {
        let a = Timestamp::from(3_i64);
        assert_eq!(a, a.clone());
        assert_eq!(a.millis(), 3);
        assert_eq!(a.to_string(), "3");
        assert!(!format!("{a:?}").is_empty());
        assert!(a < Timestamp(4));
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&Timestamp(3)));
    }
}
