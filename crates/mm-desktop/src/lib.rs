//! Pure desktop-integration logic.
//!
//! **Inside the coverage gate, at 100%.** The decisions live here — whether a
//! post should notify, where a `mattermost://` link points — and the D-Bus and
//! Secret Service calls that act on them live in `mm-desktop-xdg`, outside the
//! gate. That split is what lets every notification rule be tested in CI,
//! which has no session bus at all.

pub mod control;
pub mod deeplink;
pub mod download;
pub mod entry;
pub mod geometry;
pub mod notify;
pub mod tray;

pub use control::render_control;
pub use deeplink::{DeepLink, DeepLinkError, parse as parse_deep_link};
pub use download::{destination, safe_name};
pub use entry::{
    APP_ID, Badge, render as render_desktop_entry, render_autostart as render_autostart_entry,
};
pub use geometry::{Geometry, Restored};
pub use notify::{
    DesktopNotify, Notification, NotifySettings, Suppressed, notification_for, summarize,
};
pub use tray::{Tray, TrayAction};
