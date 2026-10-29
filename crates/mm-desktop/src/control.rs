//! The `.deb` control file.
//!
//! **Pure, and inside the coverage gate.** Building the package is
//! `xtask package --deb`'s job; what the package *claims about itself*
//! is a decision, and a wrong one fails in the least helpful way
//! there is -- at install time, on somebody else's machine, with a
//! dependency solver's error message.

/// What the binary needs to run at all.
///
/// Named rather than computed from the ELF. `dpkg-shlibdeps` would
/// derive the exact set, but it wants a full `debian/` tree and a
/// build inside it; these four pull in everything underneath them,
/// which is what a human would have written anyway.
const DEPENDS: &[&str] = &[
    "libc6",
    "libgtk-4-1",
    "libadwaita-1-0",
    "libgtksourceview-5-0",
    // Every button in the window is a symbolic icon from this theme,
    // and GTK draws a broken-image placeholder for one it cannot find
    // -- seen in the lab's container before the theme was put in it,
    // where the whole toolbar was placeholders. A desktop has it; a
    // container or a bare server does not.
    "adwaita-icon-theme",
    // The theme's icons are SVGs, and GTK loads them through the pixbuf
    // SVG loader, which is this package: without it every icon from the
    // theme is the same placeholder while GTK's own bundled ones draw,
    // which is what the lab's container showed with the theme installed.
    "librsvg2-common",
];

/// What makes it work properly, without which it still runs.
///
/// Spelling has no dictionaries of its own and simply draws no
/// underlines without these; the tray needs an `AppIndicator`
/// extension on GNOME, since GNOME 46 dropped `StatusNotifierItem`
/// from the shell. Neither is a reason to refuse to install.
const RECOMMENDS: &[&str] = &["hunspell-en-us", "gnome-shell-extension-appindicator"];

/// The `control` file's contents.
///
/// `installed_size` is in **kibibytes**, which is what the field means
/// and is the kind of detail that is wrong in half the hand-written
/// control files in the world.
#[must_use]
pub fn render_control(version: &str, arch: &str, installed_size_kib: u64) -> String {
    format!(
        "Package: mattermost-rs\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: {maintainer}\n\
         Installed-Size: {installed_size_kib}\n\
         Depends: {depends}\n\
         Recommends: {recommends}\n\
         Section: net\n\
         Priority: optional\n\
         Homepage: https://github.com/Legion112/mattermost-rs\n\
         Description: Native Linux client for Mattermost\n\
         \x20A GTK4 client for Mattermost servers, written in Rust. Not affiliated\n\
         \x20with Mattermost, Inc.\n",
        maintainer = MAINTAINER,
        depends = DEPENDS.join(", "),
        recommends = RECOMMENDS.join(", "),
    )
}

/// Debian wants a name and an address, and refuses the package
/// without one.
const MAINTAINER: &str = "legion <ageev.maks2014@gmail.com>";

#[cfg(test)]
#[path = "tests/control.rs"]
mod tests;
