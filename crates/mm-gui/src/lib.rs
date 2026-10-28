//! GTK4 + libadwaita shell.
//!
//! **Outside the coverage gate.** A thin projection of `mm-core` state: a
//! reviewer should be able to ask "where is the logic?" of any file here and
//! get the answer "it is not, it is in mm-core". The window computes nothing
//! about unread, ordering or labels — it reads `Store::sidebar()` and
//! `Store::channel_posts()` and draws them.
//!
//! Built directly on `gtk4` rather than relm4, which the plan named. relm4 is
//! a convenience layer over these same widgets, and the measured `ListView`
//! behaviour this window depends on — see `docs/gtk-notes.md` — was verified
//! against plain gtk4. Adopting relm4 is mechanical once the component tree
//! grows past one window; the design rules that matter (one store on the main
//! thread, one bridge, children send commands) are framework-independent and
//! are enforced here already.
//!
//! Targets the **GTK 4.14 / libadwaita 1.5** API level, which is what apt ships
//! on Ubuntu 24.04 and what the CI runner has. Not 4.22: that is Homebrew's,
//! and building against it yields a binary that runs on one machine only.

mod app;
pub mod bridge;
mod login;
mod perf;
mod row;
mod spelling;

pub use app::{Wiring, run};
pub use bridge::{Command, Event};

/// The success value of `glib::ExitCode`, so the binary can compare against it
/// without depending on gtk itself.
#[must_use]
pub fn glib_exit_success() -> gtk::glib::ExitCode {
    gtk::glib::ExitCode::SUCCESS
}
