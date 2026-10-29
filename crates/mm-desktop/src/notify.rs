//! Whether a post should raise a desktop notification.
//!
//! This is the decision; posting the notification is `mm-desktop-xdg`'s job.
//! Keeping them apart is what makes every rule here testable without a session
//! bus — and CI has no session bus at all.
//!
//! The rules are ordered so the cheapest and most certain come first. The one
//! that was not certain is now settled by the documentation: see
//! [`NotifySettings::muted_is_silent`].

use mm_types::{ChannelId, Post, UserId};

/// A `desktop` notify-prop value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DesktopNotify {
    /// Every message.
    All,
    /// Only mentions.
    Mention,
    /// Nothing.
    Silent,
    /// Defer to the next level up. This is the wire default for a channel that
    /// has never been configured, and it is why the effective value has to be
    /// resolved rather than read.
    #[default]
    Inherit,
}

impl DesktopNotify {
    /// Parse a `notify_props.desktop` value.
    ///
    /// An unrecognised value inherits rather than silencing: a future server
    /// value must not quietly stop notifying.
    #[must_use]
    pub fn from_prop(raw: &str) -> Self {
        match raw {
            "all" => Self::All,
            "mention" => Self::Mention,
            "none" => Self::Silent,
            _ => Self::Inherit,
        }
    }
}

/// Everything the decision needs, gathered by the caller.
//
// Four bools, which clippy dislikes. They stay: each is an independently
// settable policy switch with a distinct meaning, and `muted` in particular is
// a *fact from the server* while `muted_is_silent` is *our policy about that
// fact*. Collapsing them into an enum would conflate the two, and a bitfield
// would trade four self-describing names for one opaque one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct NotifySettings {
    /// The user's global `(notifications, desktop)` preference.
    pub global: DesktopNotify,
    /// This channel's `notify_props.desktop`.
    pub channel: DesktopNotify,
    /// Whether the channel is muted (`mark_unread == "mention"`).
    pub muted: bool,
    /// Whether muting should also silence notifications.
    ///
    /// Confirmed against Mattermost's own documentation: once a channel is
    /// muted, "all notifications for that channel are disabled, including
    /// email, desktop, incoming call ring tones, and push notifications" --
    /// a direct mention still bolds the row in the sidebar, but it does not
    /// notify. On the wire muting is `mark_unread: "mention"`, which is about
    /// the unread marker, and the webapp sets `desktop: "mention"` beside it;
    /// this flag is what makes the documented behaviour a decision of this
    /// client rather than a side effect of that second field. Default `true`.
    pub muted_is_silent: bool,
    /// Notify even when the window already shows this channel.
    pub notify_when_focused: bool,
    /// Suppress join/leave and other `system_*` posts.
    pub suppress_system: bool,
    /// Whether a notification should make a sound.
    ///
    /// The user's `notify_props.desktop_sound`, which Mattermost
    /// writes as the string `"true"` or `"false"` and defaults to on.
    /// Kept separate from whether to notify at all, because they are
    /// separate settings there and somebody who turned the sound off
    /// still wants the banner.
    pub sound: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            global: DesktopNotify::All,
            channel: DesktopNotify::Inherit,
            muted: false,
            muted_is_silent: true,
            notify_when_focused: false,
            suppress_system: true,
            sound: true,
        }
    }
}

impl NotifySettings {
    /// The channel's setting, falling back to the global one.
    #[must_use]
    pub fn effective(&self) -> DesktopNotify {
        match self.channel {
            DesktopNotify::Inherit => match self.global {
                // A global that is itself "inherit" means the user never chose,
                // which the server treats as "all".
                DesktopNotify::Inherit => DesktopNotify::All,
                chosen => chosen,
            },
            chosen => chosen,
        }
    }
}

/// What to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub channel: ChannelId,
    /// True when the reader was named, which callers use to pick urgency.
    pub is_mention: bool,
    /// Whether to make a sound. A client with banners switched off at
    /// the desktop still hears this, which for most people is the
    /// whole notification.
    pub sound: bool,
}

/// Why no notification was raised. Returned rather than a bare `None` so that
/// "why didn't it notify?" is answerable from a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppressed {
    OwnPost,
    SystemMessage,
    Deleted,
    Muted,
    SettingIsNone,
    MentionsOnlyAndNotMentioned,
    AlreadyLooking,
}

/// Decide.
///
/// `mentioned` is the caller's answer to "was the reader named?". It comes
/// from the server where possible — the `posted` event carries a `mentions`
/// list on the copy sent to a mentioned user — because the server's own
/// mention matching is the one the badge agrees with.
pub fn notification_for(
    post: &Post,
    me: &UserId,
    author_name: &str,
    channel_name: &str,
    mentioned: bool,
    focused_channel: Option<&ChannelId>,
    settings: &NotifySettings,
) -> Result<Notification, Suppressed> {
    if post.user_id == *me {
        return Err(Suppressed::OwnPost);
    }
    if post.is_deleted() {
        return Err(Suppressed::Deleted);
    }
    if settings.suppress_system && post.is_system() {
        return Err(Suppressed::SystemMessage);
    }
    // Mentions included: Mattermost's documentation has a muted channel
    // sending no notification of any kind, with a direct mention only
    // bolding the row in the sidebar. This client let a mention through
    // a mute until the lab caught the banner.
    if settings.muted && settings.muted_is_silent {
        return Err(Suppressed::Muted);
    }
    match settings.effective() {
        DesktopNotify::Silent => return Err(Suppressed::SettingIsNone),
        DesktopNotify::Mention if !mentioned => {
            return Err(Suppressed::MentionsOnlyAndNotMentioned);
        }
        DesktopNotify::All | DesktopNotify::Mention | DesktopNotify::Inherit => {}
    }
    if !settings.notify_when_focused && focused_channel == Some(&post.channel_id) {
        return Err(Suppressed::AlreadyLooking);
    }

    Ok(Notification {
        title: if channel_name.is_empty() {
            author_name.to_owned()
        } else {
            format!("{author_name} in {channel_name}")
        },
        body: summarize(&post.message),
        channel: post.channel_id.clone(),
        is_mention: mentioned,
        sound: settings.sound,
    })
}

/// Trim a message to something that fits in a notification bubble.
///
/// Truncation is on a character boundary and at a word where possible: cutting
/// a UTF-8 sequence in half panics, and cutting mid-word looks broken.
#[must_use]
pub fn summarize(message: &str) -> String {
    const LIMIT: usize = 180;
    let flat = message.replace('\n', " ");
    let trimmed = flat.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(LIMIT).collect();
    let at = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", &cut[..at].trim_end())
}

#[cfg(test)]
#[path = "tests/notify.rs"]
mod tests;
