//! XDG implementations of the `mm-desktop` decisions.
//!
//! **Outside the coverage gate.** Nothing here is decidable without a session
//! bus, and CI has none. Whether a post *should* notify is decided in
//! `mm-desktop`, which is gated and fully tested; this crate only performs the
//! call.

use mm_desktop::{Badge, Notification};

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
        // The installed icon's name, not the application's: the shell
        // looks this up in the icon theme, and "mattermost-rs" is not
        // in it -- which is why the test notification has been drawing
        // a generic placeholder.
        .icon(mm_desktop::APP_ID);

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

    // A "default" action is what makes clicking the banner itself mean
    // something. Without one, most shells treat the notification as
    // nothing but a thing to dismiss, and clicking it does nothing --
    // which reads as the client ignoring you.
    builder.action("default", "Open");

    builder
        .show()
        .map(|_| ())
        .map_err(|e| format!("could not post a notification: {e}"))
}

/// Show a notification and wait for the person to act on it.
///
/// **Blocks until the notification is dismissed or clicked**, so it
/// belongs on a thread of its own. Answers whether it was clicked
/// rather than merely closed, which is the difference between "take me
/// there" and "go away".
///
/// Separate from [`show`] rather than replacing it: a caller with
/// nowhere to send the answer should not be made to find a thread.
pub fn show_and_wait(notification: &Notification) -> Result<bool, String> {
    let mut builder = notify_rust::Notification::new();
    builder
        .summary(&notification.title)
        .body(&notification.body)
        .appname("mattermost-rs")
        .icon(mm_desktop::APP_ID)
        .action("default", "Open")
        .urgency(if notification.is_mention {
            notify_rust::Urgency::Normal
        } else {
            notify_rust::Urgency::Low
        })
        .hint(notify_rust::Hint::Custom(
            "x-mattermost-rs-channel".into(),
            notification.channel.to_string(),
        ));

    let handle = builder
        .show()
        .map_err(|e| format!("could not post a notification: {e}"))?;
    let clicked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let answer = std::sync::Arc::clone(&clicked);
    handle.wait_for_action(move |action| {
        // `__closed` is the shell saying it went away on its own, or
        // that the person swiped it aside. Only a real action means
        // they want to be taken somewhere.
        if action != "__closed" {
            answer.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    Ok(clicked.load(std::sync::atomic::Ordering::Relaxed))
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

/// Put the unread count on the application's icon in the dock.
///
/// This is the `com.canonical.Unity.LauncherEntry` protocol -- a plain
/// signal on the session bus, no service to own and nothing to
/// register. Ubuntu's dock, Plank, Latte and the Dash-to-Dock
/// extension all listen for it; a desktop that does not simply ignores
/// it, which is why nothing here treats absence as an error.
///
/// **It only lands if the window can be matched to a launcher.** The
/// URI names the `.desktop` file, and the desktop matches the window to
/// it by `StartupWMClass`; with no entry installed, or with the app id
/// and `StartupWMClass` out of step, the signal is emitted into
/// nothing. `cargo xtask install-desktop` is what puts that entry
/// there.
///
/// A connection per call rather than one kept open: this fires when a
/// mention count changes, which is rarely, and a long-lived connection
/// would be one more thing to reconnect after the bus restarts.
pub fn set_badge(badge: Badge) -> Result<(), String> {
    let uri = format!("application://{id}.desktop", id = mm_desktop::APP_ID);
    let properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> = [
        ("count", zbus::zvariant::Value::I64(badge.count)),
        ("count-visible", zbus::zvariant::Value::Bool(badge.visible)),
    ]
    .into_iter()
    .collect();

    let connection =
        zbus::blocking::Connection::session().map_err(|e| format!("no session bus: {e}"))?;
    connection
        .emit_signal(
            None::<&str>,
            "/",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(uri.as_str(), properties),
        )
        .map_err(|e| format!("could not set the badge: {e}"))
}
