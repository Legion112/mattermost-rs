//! XDG implementations of the `mm-desktop` decisions.
//!
//! **Outside the coverage gate.** Nothing here is decidable without a session
//! bus, and CI has none. Whether a post *should* notify is decided in
//! `mm-desktop`, which is gated and fully tested; this crate only performs the
//! call.

use mm_desktop::Notification;

/// Show a desktop notification.
///
/// Failure is returned rather than panicked on, and callers are expected to
/// carry on: a missing notification daemon is a perfectly normal state on a
/// minimal desktop, and it is not a reason for a chat client to stop working.
pub fn show(notification: &Notification) -> Result<(), String> {
    let mut builder = notify_rust::Notification::new();
    builder
        .summary(&notification.title)
        .body(&notification.body)
        .appname("mattermost-rs")
        .icon("mattermost-rs");

    // A mention is the one thing worth interrupting for, so it gets normal
    // urgency and everything else gets low. Critical is reserved for things
    // that do not go away on their own, which a chat message is not.
    builder.urgency(if notification.is_mention {
        notify_rust::Urgency::Normal
    } else {
        notify_rust::Urgency::Low
    });

    // Collapse repeated notifications from the same channel rather than
    // stacking twenty of them during a busy thread.
    builder.hint(notify_rust::Hint::Custom(
        "x-mattermost-rs-channel".into(),
        notification.channel.to_string(),
    ));

    builder
        .show()
        .map(|_| ())
        .map_err(|e| format!("could not post a notification: {e}"))
}

/// Whether a notification daemon is reachable at all.
///
/// Worth asking once at startup so the app can say "notifications are
/// unavailable" rather than silently never notifying — which looks identical
/// to a bug in the notification rules.
#[must_use]
pub fn is_available() -> bool {
    notify_rust::get_capabilities().is_ok()
}
