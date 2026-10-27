//! What the tray icon says, and what its menu offers.
//!
//! **Pure, and inside the coverage gate.** The `StatusNotifierItem`
//! plumbing lives in `mm-desktop-xdg`; what the thing *says* is a
//! decision, and decisions are tested.
//!
//! A tray on GNOME needs an `AppIndicator` extension, since GNOME 46
//! dropped `StatusNotifierItem` from the shell itself. Ubuntu ships and
//! enables one. Everything here is written anyway, because the menu
//! model is the same on every desktop that has a tray at all.

/// What to show, given how many mentions are waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tray {
    /// The hover text. Carries the count, because a tray icon that
    /// says only the application's name wastes the one line it gets.
    pub tooltip: String,
    /// Whether to ask the shell for attention. Reserved for mentions:
    /// an icon that is always shouting is an icon nobody reads.
    pub urgent: bool,
}

/// What each menu entry does. The tray crate wants closures; this is
/// the same thing as data, so that the menu can be asserted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Bring the window to the front.
    Open,
    /// Leave properly, rather than closing the window to the tray.
    Quit,
}

impl Tray {
    #[must_use]
    pub fn for_mentions(mentions: i64) -> Self {
        let tooltip = match mentions {
            n if n <= 0 => "mattermost-rs".to_owned(),
            1 => "mattermost-rs — 1 mention".to_owned(),
            n => format!("mattermost-rs — {n} mentions"),
        };
        Self {
            tooltip,
            urgent: mentions > 0,
        }
    }

    /// The menu, in order.
    ///
    /// Two entries and no more. A tray menu is not a place to put
    /// features: it is reached by accident as often as on purpose, and
    /// everything on it is one slip away from being clicked.
    #[must_use]
    pub fn menu() -> Vec<(&'static str, TrayAction)> {
        vec![("Open", TrayAction::Open), ("Quit", TrayAction::Quit)]
    }
}

#[cfg(test)]
#[path = "tests/tray.rs"]
mod tests;
