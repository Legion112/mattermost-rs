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
}
