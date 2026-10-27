//! The `.desktop` file that makes this an application the desktop knows
//! about.
//!
//! **Pure, and inside the coverage gate.** Rendering it is a string
//! transformation; writing it to disk is `xtask install-desktop`'s job.
//! Getting it wrong is quiet and annoying rather than loud -- a wrong
//! `StartupWMClass` means the window never matches its launcher, so it
//! gets a second icon in the dock and no unread badge, and nothing
//! anywhere says why.

use std::path::Path;

/// The application id, which is also the icon's name and the GTK
/// application id. All three have to agree or the desktop cannot tell
/// that the window, the launcher and the icon are one application.
pub const APP_ID: &str = "dev.legion.mattermost_rs";

/// The `.desktop` file's contents, for a binary at `binary`.
///
/// `%u` is there for `mattermost://` links: the desktop passes the URL
/// as an argument, and without it the handler registration is accepted
/// and then silently does nothing with the link.
#[must_use]
pub fn render(binary: &Path) -> String {
    entry(binary, "")
}

/// The same entry, for the autostart directory.
///
/// Two differences, both necessary. `--hidden` starts it in the tray
/// rather than throwing a window at somebody who has just logged in
/// and is looking at something else. And `X-GNOME-Autostart-enabled`
/// is what GNOME's own Startup Applications toggles, so without it the
/// entry cannot be turned off from the settings the reader would look
/// in.
#[must_use]
pub fn render_autostart(binary: &Path) -> String {
    format!(
        "{}X-GNOME-Autostart-enabled=true\n",
        entry(binary, " --hidden")
    )
}

fn entry(binary: &Path, extra: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=mattermost-rs\n\
         GenericName=Mattermost client\n\
         Comment=A native Linux client for Mattermost\n\
         Exec={exec}{extra} %u\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n\
         SingleMainWindow=true\n\
         Categories=Network;InstantMessaging;Chat;\n\
         Keywords=chat;mattermost;messaging;\n\
         MimeType=x-scheme-handler/mattermost;\n",
        exec = quote(binary),
    )
}

/// A path as `Exec=` wants it.
///
/// The spec reserves a handful of characters, and a path with a space
/// in it -- `/home/me/My Builds/mattermost-rs` -- is read as a command
/// plus an argument unless it is quoted. Inside quotes, a backslash and
/// a quote must themselves be escaped.
fn quote(binary: &Path) -> String {
    let shown = binary.to_string_lossy();
    if !shown.contains([
        ' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`',
    ]) {
        return shown.into_owned();
    }
    let escaped: String = shown
        .chars()
        .flat_map(|c| {
            let needs = matches!(c, '"' | '`' | '$' | '\\');
            needs.then_some('\\').into_iter().chain(std::iter::once(c))
        })
        .collect();
    format!("\"{escaped}\"")
}

/// What a dock should draw on the application's icon.
///
/// Mentions only, not unread messages: the count is a number of things
/// addressed to this person, and a channel that is merely busy is not
/// one of them. That is the rule Mattermost's own badge follows, and a
/// badge that counted every unread message would read "412" on a
/// Monday and mean nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Badge {
    pub count: i64,
    /// Whether to draw it at all. A badge showing "0" is worse than no
    /// badge: it reads as a thing to attend to.
    pub visible: bool,
}

impl Badge {
    #[must_use]
    pub fn for_mentions(mentions: i64) -> Self {
        // Clamped: the count goes on the wire as an i64 and a negative
        // one would be a dock drawing "-1", which can only come from
        // arithmetic that has already gone wrong somewhere else.
        let count = mentions.max(0);
        Self {
            count,
            visible: count > 0,
        }
    }
}

#[cfg(test)]
#[path = "tests/entry.rs"]
mod tests;
