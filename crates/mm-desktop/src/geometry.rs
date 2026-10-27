//! How big the window was last time.
//!
//! **Pure, and inside the coverage gate.** A chat client that opens at
//! some default size every launch is one the reader has to drag into
//! place every launch, and half a screen is where this one lives. What
//! is remembered is the **size**, not the position: on X11 the window
//! manager owns placement, GTK4 deliberately offers no way to ask for a
//! spot, and a client that saved coordinates it cannot restore would be
//! lying in its own state file.

use serde::{Deserialize, Serialize};

/// The smallest window worth restoring. Anything under this came from a
/// window being saved mid-animation or mid-unmap, and restoring it
/// would open the client as a postage stamp.
const MIN_WIDTH: i32 = 480;
const MIN_HEIGHT: i32 = 360;
/// Bigger than any single display is a saved value from a session with
/// a monitor that is no longer attached.
const MAX_SIDE: i32 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            width: 1_040,
            height: 720,
            maximized: false,
        }
    }
}

impl Geometry {
    /// Whether this is worth writing down.
    ///
    /// A maximized window reports whatever size it had before it was
    /// maximized, which is the right thing to restore to when it is
    /// un-maximized later -- but a nonsense size next to it is not.
    #[must_use]
    pub fn worth_saving(self) -> bool {
        self.sane() == self
    }

    /// The same geometry with anything unusable replaced.
    #[must_use]
    pub fn sane(self) -> Self {
        let fix = |value: i32, min: i32, fallback: i32| {
            if (min..=MAX_SIDE).contains(&value) {
                value
            } else {
                fallback
            }
        };
        let default = Self::default();
        Self {
            width: fix(self.width, MIN_WIDTH, default.width),
            height: fix(self.height, MIN_HEIGHT, default.height),
            maximized: self.maximized,
        }
    }
}

/// Everything about the window worth carrying from one launch to the
/// next: how big it was, and where the reader was reading.
///
/// Reopening on the channel they left is most of what makes a client
/// feel like it was never closed; opening on whichever channel happens
/// to sort first means finding their place again every morning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Restored {
    #[serde(flatten)]
    pub geometry: Geometry,
    /// The channel that was open, empty when there was none. Kept as a
    /// plain string: this file outlives any particular channel, and an
    /// id that no longer exists must read as "no channel" rather than
    /// fail the whole file.
    pub channel: String,
}

impl Restored {
    /// The same, with an unusable size replaced.
    #[must_use]
    pub fn sane(self) -> Self {
        Self {
            geometry: self.geometry.sane(),
            channel: self.channel,
        }
    }
}

#[cfg(test)]
#[path = "tests/geometry.rs"]
mod tests;
