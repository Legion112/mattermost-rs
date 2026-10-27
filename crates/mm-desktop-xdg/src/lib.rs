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
///
/// Async, and deliberately not `zbus::blocking`: that builds its own
/// multi-threaded runtime, which panics when it is called from inside
/// one -- and this is called from the engine's.
pub async fn set_badge(badge: Badge) -> Result<(), String> {
    let uri = format!("application://{id}.desktop", id = mm_desktop::APP_ID);
    let properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> = [
        ("count", zbus::zvariant::Value::I64(badge.count)),
        ("count-visible", zbus::zvariant::Value::Bool(badge.visible)),
    ]
    .into_iter()
    .collect();

    let connection = zbus::Connection::session()
        .await
        .map_err(|e| format!("no session bus: {e}"))?;
    connection
        .emit_signal(
            None::<&str>,
            "/",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(uri.as_str(), properties),
        )
        .await
        .map_err(|e| format!("could not set the badge: {e}"))
}

/// The tray icon, as the `StatusNotifierItem` spec wants it.
///
/// **GNOME 46 dropped `StatusNotifierItem` from the shell**, so this only
/// appears where an `AppIndicator` extension is installed. Ubuntu ships
/// and enables one, which is why it is worth having; a desktop without
/// one simply never shows it, and nothing here treats that as failure.
///
/// What it says is decided in `mm-desktop` and tested there. This
/// carries it.
#[derive(Debug)]
pub struct TrayIcon {
    say: mm_desktop::Tray,
    /// Where a click or a menu choice goes. Unbounded because the
    /// alternative is a tray that blocks the D-Bus thread when the
    /// window is busy.
    acts: std::sync::mpsc::Sender<mm_desktop::TrayAction>,
}

impl ksni::Tray for TrayIcon {
    fn id(&self) -> String {
        mm_desktop::APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "mattermost-rs".to_owned()
    }

    /// The icon as actual pixels, and **no icon name at all**.
    ///
    /// Not an oversight. A host that is given both prefers the name,
    /// resolves it against its own idea of the icon theme, fails to
    /// find an application installed into `~/.local/share/icons`, and
    /// draws three dots -- which is exactly what Ubuntu's `AppIndicator`
    /// extension did here, with a perfectly good pixmap sitting on the
    /// bus beside the name it could not resolve. Offering only the
    /// pixels takes the lookup out of it.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        TRAY_PIXMAP.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.say.tooltip.clone(),
            ..ksni::ToolTip::default()
        }
    }

    fn status(&self) -> ksni::Status {
        if self.say.urgent {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    /// A left click opens the window, which is what a tray icon is for.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.acts.send(mm_desktop::TrayAction::Open);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        mm_desktop::Tray::menu()
            .into_iter()
            .map(|(label, action)| {
                ksni::menu::StandardItem {
                    label: label.to_owned(),
                    activate: Box::new(move |tray: &mut Self| {
                        let _ = tray.acts.send(action);
                    }),
                    ..ksni::menu::StandardItem::default()
                }
                .into()
            })
            .collect()
    }
}

/// The tray icon's pixels, decoded once.
///
/// ARGB32, which is what the `StatusNotifierItem` spec asks for and is
/// not what a PNG decodes to: the channels have to be rotated and the
/// colour premultiplied, and getting either wrong shows as an icon
/// with the red and blue swapped or a black box where the
/// transparency was.
static TRAY_PIXMAP: std::sync::LazyLock<Vec<ksni::Icon>> = std::sync::LazyLock::new(|| {
    const PNG: &[u8] = include_bytes!("../../../assets/dev.legion.mattermost_rs-44.png");
    let decoder = png::Decoder::new(PNG);
    let Ok(mut reader) = decoder.read_info() else {
        return Vec::new();
    };
    let mut rgba = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut rgba) else {
        return Vec::new();
    };
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Vec::new();
    }
    let argb = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = u32::from(p[3]);
            // Premultiplied, as the spec wants: a half-transparent
            // pixel carries half its colour, or the edges fringe.
            let mul = |c: u8| u8::try_from(u32::from(c) * a / 255).unwrap_or(c);
            [p[3], mul(p[0]), mul(p[1]), mul(p[2])]
        })
        .collect();
    vec![ksni::Icon {
        width: i32::try_from(info.width).unwrap_or(0),
        height: i32::try_from(info.height).unwrap_or(0),
        data: argb,
    }]
});

/// A live tray icon, for updating what it says.
///
/// Aliased so that nothing outside this crate has to name the tray
/// library: which one it is, is this crate's business.
pub type TrayHandle = ksni::Handle<TrayIcon>;

/// Put an icon in the tray, if there is a tray.
///
/// Answers a handle for updating the count, or why there is no tray.
/// The caller must treat a failure as "no tray" rather than as
/// something to stop for -- but it is worth *saying*, because a tray
/// that silently never appears looks exactly like one that is broken,
/// and closing the window to a tray that is not there would hide the
/// application somewhere unreachable.
pub async fn show_tray(
    acts: std::sync::mpsc::Sender<mm_desktop::TrayAction>,
) -> Result<TrayHandle, String> {
    use ksni::TrayMethods as _;
    TrayIcon {
        say: mm_desktop::Tray::for_mentions(0),
        acts,
    }
    .spawn()
    .await
    .map_err(|e| format!("no tray: {e}"))
}

/// Tell the tray how many mentions are waiting.
pub async fn update_tray(handle: &TrayHandle, mentions: i64) {
    handle
        .update(|tray| tray.say = mm_desktop::Tray::for_mentions(mentions))
        .await;
}
