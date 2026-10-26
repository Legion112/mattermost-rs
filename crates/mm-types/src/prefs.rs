//! User preferences.
//!
//! The server models these as a flat `(category, name) -> value` table with
//! string values, and this crate mirrors that rather than imposing a typed
//! struct. Mattermost adds preference categories regularly; a typed struct
//! would need a change here every time, and an unknown category would have
//! nowhere to go.

use crate::UserId;
use serde::{Deserialize, Serialize};

/// A single preference row.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default)]
pub struct Preference {
    pub user_id: UserId,
    pub category: String,
    pub name: String,
    pub value: String,
}

/// Preference categories and names the client actually reads.
pub mod keys {
    /// Category for display settings.
    pub const DISPLAY_SETTINGS: &str = "display_settings";
    /// Within [`DISPLAY_SETTINGS`]: whether collapsed reply threads are on for
    /// this user. Only consulted when the server's `CollapsedThreads` setting
    /// is `default_on` or `default_off`.
    pub const COLLAPSED_REPLY_THREADS: &str = "collapsed_reply_threads";
    /// Within [`DISPLAY_SETTINGS`]: how to render other people's names.
    pub const NAME_FORMAT: &str = "name_format";

    /// Category for notification settings.
    pub const NOTIFICATIONS: &str = "notifications";

    /// Category whose rows mark favourited channels; `name` is the channel id.
    pub const FAVORITE_CHANNEL: &str = "favorite_channel";

    /// Category whose rows mark saved posts; `name` is the post id.
    pub const FLAGGED_POST: &str = "flagged_post";

    /// Category for whether a DM channel is shown in the sidebar; `name` is
    /// the other user's id.
    pub const DIRECT_CHANNEL_SHOW: &str = "direct_channel_show";
}

/// How to render another user's name, from the `name_format` preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NameFormat {
    /// `username` — the default when the preference is unset.
    #[default]
    Username,
    /// `nickname_full_name` — nickname, falling back to full name.
    NicknameFullName,
    /// `full_name` — first and last.
    FullName,
    /// `show_fullname` is the older spelling of `full_name`, still seen.
    ShowFullName,
}

impl NameFormat {
    /// Parse a `name_format` value, treating anything unrecognised as the
    /// default rather than failing — the preference is cosmetic, and refusing
    /// to start over an unknown value would be absurd.
    #[must_use]
    pub fn from_value(raw: &str) -> Self {
        match raw {
            "nickname_full_name" => Self::NicknameFullName,
            "full_name" => Self::FullName,
            "show_fullname" => Self::ShowFullName,
            _ => Self::Username,
        }
    }
}

/// Whether a preference value means "on".
///
/// The server stores booleans as the strings `"true"` and `"false"`, and an
/// unset preference arrives as an absent row rather than `"false"`.
#[must_use]
pub fn is_true(value: &str) -> bool {
    value == "true"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_round_trips() {
        let p = Preference {
            user_id: UserId::new("u1"),
            category: keys::DISPLAY_SETTINGS.into(),
            name: keys::COLLAPSED_REPLY_THREADS.into(),
            value: "on".into(),
        };
        let round: Preference = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(round, p);
        assert_eq!(p, p.clone());
        assert!(!format!("{p:?}").is_empty());
        assert!(p <= round);
    }

    #[test]
    fn absent_preference_fields_default() {
        let p: Preference = serde_json::from_str(r#"{"category":"x"}"#).unwrap();
        assert_eq!(p.category, "x");
        assert_eq!(p.value, "");
        assert_eq!(Preference::default().name, "");
    }

    #[test]
    fn name_format_parses_every_spelling_and_defaults_otherwise() {
        assert_eq!(
            NameFormat::from_value("nickname_full_name"),
            NameFormat::NicknameFullName
        );
        assert_eq!(NameFormat::from_value("full_name"), NameFormat::FullName);
        assert_eq!(
            NameFormat::from_value("show_fullname"),
            NameFormat::ShowFullName
        );
        for raw in ["username", "", "something_new"] {
            assert_eq!(NameFormat::from_value(raw), NameFormat::Username, "{raw}");
        }
        assert_eq!(NameFormat::default(), NameFormat::Username);
        let f = NameFormat::FullName;
        assert_eq!(f, f.clone());
        assert!(!format!("{f:?}").is_empty());
    }

    /// Booleans are the literal strings "true"/"false", and anything else —
    /// including the common "on" — is not a boolean.
    #[test]
    fn is_true_only_accepts_the_literal_string() {
        assert!(is_true("true"));
        for raw in ["false", "on", "1", "", "TRUE"] {
            assert!(!is_true(raw), "{raw}");
        }
    }
}
