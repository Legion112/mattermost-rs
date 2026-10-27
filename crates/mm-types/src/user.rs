//! Users, teams and presence.

use crate::{NameFormat, TeamId, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A user, as sanitized by the server for the requesting caller.
///
/// Which fields are populated depends on who is asking: `email` is commonly
/// absent, and the login response returns a more complete record than a
/// roster fetch does. Everything is therefore defaulted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub nickname: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub roles: String,
    pub locale: String,
    pub create_at: Timestamp,
    pub update_at: Timestamp,
    /// Non-zero means deactivated. Their posts still render.
    pub delete_at: Timestamp,
    /// Changes whenever the avatar changes, which makes it a free cache key
    /// for the avatar file — include it in the filename and invalidation is
    /// automatic.
    pub last_picture_update: Timestamp,
    pub is_bot: bool,
    pub notify_props: BTreeMap<String, String>,
}

impl User {
    #[must_use]
    pub fn is_deactivated(&self) -> bool {
        !self.delete_at.is_zero()
    }

    /// What to show in a post header.
    ///
    /// Mattermost has a "teammate name display" preference with four settings;
    /// this is the nickname-then-full-name-then-username fallback, which is
    /// the most common configuration. Honouring the preference properly is a
    /// `mm-core` concern because it needs the preference store.
    #[must_use]
    pub fn display_name(&self) -> &str {
        if !self.nickname.is_empty() {
            return &self.nickname;
        }
        if !self.first_name.is_empty() {
            return &self.first_name;
        }
        &self.username
    }

    /// The name to show, in the format the server or the user asked for.
    ///
    /// Separate from `display_name`, which is the old unconditional
    /// nickname-then-first-name-then-username rule. That rule is one of the
    /// four the server supports, and not the default: Mattermost ships
    /// `TeammateNameDisplay = username`, so ignoring the setting showed
    /// people a roster of names nobody else in their organisation sees.
    ///
    /// Every arm falls back to the username, which is the one field that is
    /// always set and always unique.
    #[must_use]
    pub fn name_as(&self, format: NameFormat) -> String {
        let fallback = || self.username.clone();
        // A bot is shown by its display name whatever the setting says,
        // which is what Mattermost does. The bot record's display name is
        // kept in the account's `first_name`, so a bot called "System" has
        // the username `system-bot` and showing that is simply wrong --
        // and wrong invisibly for a bot whose two names happen to match.
        if self.is_bot && !self.first_name.is_empty() {
            return self.first_name.clone();
        }
        match format {
            NameFormat::Username => fallback(),
            NameFormat::NicknameFullName => {
                if !self.nickname.is_empty() {
                    return self.nickname.clone();
                }
                let full = self.full_name();
                if full.is_empty() { fallback() } else { full }
            }
            NameFormat::FullName | NameFormat::ShowFullName => {
                let full = self.full_name();
                if full.is_empty() { fallback() } else { full }
            }
        }
    }

    #[must_use]
    pub fn full_name(&self) -> String {
        match (self.first_name.is_empty(), self.last_name.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.first_name.clone(),
            (true, false) => self.last_name.clone(),
            (false, false) => format!("{} {}", self.first_name, self.last_name),
        }
    }

    #[must_use]
    pub fn is_system_admin(&self) -> bool {
        self.roles.split_whitespace().any(|r| r == "system_admin")
    }
}

/// A team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Team {
    pub id: TeamId,
    pub create_at: Timestamp,
    pub update_at: Timestamp,
    pub delete_at: Timestamp,
    /// URL-safe name; this is what appears in a permalink.
    pub name: String,
    pub display_name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// Presence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Online,
    Away,
    Dnd,
    #[default]
    Offline,
}

impl Presence {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Away => "away",
            Self::Dnd => "dnd",
            Self::Offline => "offline",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_falls_back_nickname_then_first_then_username() {
        let mut u = User {
            username: "jdoe".into(),
            ..User::default()
        };
        assert_eq!(u.display_name(), "jdoe");
        u.first_name = "Jane".into();
        assert_eq!(u.display_name(), "Jane");
        u.nickname = "jd".into();
        assert_eq!(u.display_name(), "jd");
    }

    #[test]
    fn full_name_handles_every_combination_of_missing_halves() {
        let mut u = User::default();
        assert_eq!(u.full_name(), "");
        u.first_name = "Jane".into();
        assert_eq!(u.full_name(), "Jane");
        u.last_name = "Doe".into();
        assert_eq!(u.full_name(), "Jane Doe");
        u.first_name.clear();
        assert_eq!(u.full_name(), "Doe");
    }

    /// A deactivated user's posts still render, so this must be a question the
    /// client can ask rather than a reason to drop the record.
    #[test]
    fn deactivation_is_a_nonzero_delete_at() {
        let mut u = User::default();
        assert!(!u.is_deactivated());
        u.delete_at = Timestamp(1);
        assert!(u.is_deactivated());
    }

    /// Roles are a space-separated string, so substring matching would make
    /// `system_administrator` look like `system_admin`.
    #[test]
    fn system_admin_matches_a_whole_role_not_a_substring() {
        let admin = User {
            roles: "system_user system_admin".into(),
            ..User::default()
        };
        assert!(admin.is_system_admin());

        let not = User {
            roles: "system_user system_admin_something".into(),
            ..User::default()
        };
        assert!(!not.is_system_admin());
        assert!(!User::default().is_system_admin());
    }

    #[test]
    fn unknown_user_fields_are_ignored_and_absent_ones_default() {
        let u: User =
            serde_json::from_str(r#"{"id":"u1","username":"a","field_from_the_future":[1,2,3]}"#)
                .unwrap();
        assert_eq!(u.username, "a");
        assert_eq!(u.email, "");
        assert!(!u.is_bot);
    }

    #[test]
    fn presence_round_trips_through_its_lowercase_wire_form() {
        for p in [
            Presence::Online,
            Presence::Away,
            Presence::Dnd,
            Presence::Offline,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Presence>(&json).unwrap(), p);
        }
        assert_eq!(Presence::default(), Presence::Offline);
    }

    #[test]
    fn every_derive_is_exercised() {
        let u = User::default();
        let t = Team::default();
        let p = Presence::default();
        assert_eq!(u, u.clone());
        assert_eq!(t, t.clone());
        assert_eq!(p, p.clone());
        for s in [format!("{u:?}"), format!("{t:?}"), format!("{p:?}")] {
            assert!(!s.is_empty());
        }
        let round: Team = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(round, t);
        let round: User = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(round, u);
    }
}

#[cfg(test)]
mod name_format_tests {
    use super::*;

    fn person() -> User {
        User {
            username: "lowroar".into(),
            nickname: "Паша".into(),
            first_name: "Павел".into(),
            last_name: "Иванов".into(),
            ..User::default()
        }
    }

    /// The server's default, and the one that was being ignored: Mattermost
    /// ships `TeammateNameDisplay = username`, so showing a nickname instead
    /// gave people a roster of names nobody else in their organisation sees.
    #[test]
    fn username_is_the_default_and_wins_outright() {
        assert_eq!(person().name_as(NameFormat::Username), "lowroar");
        assert_eq!(NameFormat::default(), NameFormat::Username);
    }

    /// Nickname first, then the full name, then the username.
    #[test]
    fn nickname_full_name_walks_down_to_something_that_exists() {
        let p = person();
        assert_eq!(p.name_as(NameFormat::NicknameFullName), "Паша");

        let no_nickname = User {
            nickname: String::new(),
            ..person()
        };
        assert_eq!(
            no_nickname.name_as(NameFormat::NicknameFullName),
            "Павел Иванов"
        );

        let bare = User {
            username: "lowroar".into(),
            ..User::default()
        };
        assert_eq!(bare.name_as(NameFormat::NicknameFullName), "lowroar");
    }

    /// `show_fullname` is the older spelling and must behave identically, or
    /// an account that set the preference years ago reads differently from
    /// one that set it last week.
    #[test]
    fn both_spellings_of_full_name_agree() {
        let p = person();
        assert_eq!(p.name_as(NameFormat::FullName), "Павел Иванов");
        assert_eq!(p.name_as(NameFormat::ShowFullName), "Павел Иванов");

        // With no name set at all, both still produce something usable.
        let bare = User {
            username: "lowroar".into(),
            ..User::default()
        };
        assert_eq!(bare.name_as(NameFormat::FullName), "lowroar");
        assert_eq!(bare.name_as(NameFormat::ShowFullName), "lowroar");
    }

    /// Only a first name, or only a last one, is a real state.
    #[test]
    fn a_half_filled_name_is_still_a_name() {
        let first_only = User {
            username: "lowroar".into(),
            first_name: "Павел".into(),
            ..User::default()
        };
        assert_eq!(first_only.name_as(NameFormat::FullName), "Павел");

        let last_only = User {
            username: "lowroar".into(),
            last_name: "Иванов".into(),
            ..User::default()
        };
        assert_eq!(last_only.name_as(NameFormat::FullName), "Иванов");
    }
}

#[cfg(test)]
mod bot_name_tests {
    use super::*;

    fn bot(username: &str, display: &str) -> User {
        User {
            username: username.into(),
            first_name: display.into(),
            is_bot: true,
            ..User::default()
        }
    }

    /// The bug this exists for: a DM with the `System` bot was listed as
    /// `system-bot`, because the server's name format is `username` and
    /// this followed it faithfully. Mattermost does not: a bot is its
    /// display name under every format.
    #[test]
    fn a_bot_is_its_display_name_under_every_format() {
        let system = bot("system-bot", "System");
        for format in [
            NameFormat::Username,
            NameFormat::NicknameFullName,
            NameFormat::FullName,
            NameFormat::ShowFullName,
        ] {
            assert_eq!(system.name_as(format), "System", "{format:?}");
        }
    }

    /// The case that hides the bug: a bot whose two names match reads
    /// correctly either way, which is why only one of two bots looked wrong.
    #[test]
    fn a_bot_whose_names_match_reads_the_same_either_way() {
        assert_eq!(
            bot("deploy-info", "deploy-info").name_as(NameFormat::Username),
            "deploy-info"
        );
    }

    /// A bot with no display name still has to be called something.
    #[test]
    fn a_nameless_bot_falls_back_to_its_username() {
        assert_eq!(
            bot("quiet-bot", "").name_as(NameFormat::Username),
            "quiet-bot"
        );
    }

    /// And a person is unaffected: the rule is about bots only.
    #[test]
    fn a_person_still_follows_the_servers_format() {
        let person = User {
            username: "lowroar".into(),
            first_name: "Павел".into(),
            ..User::default()
        };
        assert_eq!(person.name_as(NameFormat::Username), "lowroar");
        assert_eq!(person.name_as(NameFormat::FullName), "Павел");
    }
}
