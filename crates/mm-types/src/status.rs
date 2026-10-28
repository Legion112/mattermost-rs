//! Whether somebody is at their desk.

use crate::ids::UserId;
use crate::time::Timestamp;
use crate::user::Presence;
use serde::{Deserialize, Serialize};

/// One person's presence, as the status endpoints return it.
///
/// `status` is kept as the string the server sent and turned into a
/// [`Presence`] on the way out, rather than deserialized into the enum
/// directly: presence is decoration, and a server that grows a fifth
/// state must not fail the whole batch and leave every dot blank.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub user_id: UserId,
    pub status: String,
    pub manual: bool,
    pub last_activity_at: Timestamp,
    /// When a timed do-not-disturb ends, in **seconds** since the epoch
    /// -- not the milliseconds everything else on the wire uses -- and
    /// zero when there is no end to it.
    #[serde(default)]
    pub dnd_end_time: i64,
}

impl Status {
    #[must_use]
    pub fn presence(&self) -> Presence {
        Presence::parse(&self.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anything the client does not recognise reads as offline, because
    /// a dot nobody can explain is better than no window.
    #[test]
    fn a_status_becomes_a_presence_and_anything_unknown_is_offline() {
        let status: Status =
            serde_json::from_str(r#"{"user_id":"u1","status":"away","manual":true}"#).unwrap();
        assert_eq!(status.presence(), Presence::Away);
        assert_eq!(status.user_id, UserId::new("u1"));
        assert!(status.manual);

        let odd = Status {
            status: "in_a_meeting".into(),
            ..Status::default()
        };
        assert_eq!(odd.presence(), Presence::Offline);
        assert_eq!(Status::default().presence(), Presence::Offline);
    }

    /// The end of a timed quiet comes in seconds, and a status without
    /// one says zero rather than nothing.
    #[test]
    fn a_timed_quiet_says_when_it_ends() {
        let timed: Status = serde_json::from_str(
            r#"{"user_id":"u1","status":"dnd","manual":true,"last_activity_at":0,"dnd_end_time":1730000000}"#,
        )
        .expect("parses");
        assert_eq!(timed.dnd_end_time, 1_730_000_000);
        assert_eq!(timed.presence(), Presence::Dnd);
        let plain: Status =
            serde_json::from_str(r#"{"user_id":"u1","status":"online"}"#).expect("parses");
        assert_eq!(plain.dnd_end_time, 0);
    }
}
