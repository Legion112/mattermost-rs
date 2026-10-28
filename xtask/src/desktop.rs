//! `cargo xtask install-desktop` — make this an application the desktop
//! knows about, and `uninstall-desktop` to take it away again.
//!
//! Into the user's own XDG directories rather than `/usr`: no root, no
//! package manager, and uninstalling is deleting two files. The `.deb`
//! will put the same two files in the system directories when there is
//! a `.deb`.
//!
//! What this buys, beyond a menu entry: the window matches a launcher,
//! which is what lets a dock draw an unread badge on it, and
//! `mattermost://` links start resolving to this client.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the icon and the entry go, relative to the data home.
const ICON_DIR: &str = "icons/hicolor/scalable/apps";
const ENTRY_DIR: &str = "applications";

pub(crate) fn install(args: &[String]) -> Result<(), String> {
    let autostart = args.iter().any(|a| a == "--autostart");
    let binary = built_binary()?;
    let icon = icon_path()?;
    let entry = entry_path()?;

    let source = repo_root()
        .join("assets")
        .join(format!("{id}.svg", id = mm_desktop::APP_ID));
    let svg =
        std::fs::read(&source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    write(&icon, &svg)?;
    write(&entry, mm_desktop::render_desktop_entry(&binary).as_bytes())?;

    if autostart {
        write(
            &autostart_path()?,
            mm_desktop::render_autostart_entry(&binary).as_bytes(),
        )?;
    }

    println!("installed:");
    println!("  {}", entry.display());
    println!("  {}", icon.display());
    if autostart {
        println!("  {}", autostart_path()?.display());
    }
    println!("  pointing at {}", binary.display());
    refresh_caches();
    println!(
        "\nmattermost-rs should now be in the applications menu, and\n\
         mattermost:// links should open it. If the dock shows two icons,\n\
         the window's app id and StartupWMClass have drifted apart."
    );
    Ok(())
}

/// Exactly what an older `install-desktop` wrote into the shared icon
/// theme. Matched byte for byte so that `uninstall` can take back that
/// mistake and can never delete a file somebody else put there.
const OLD_THEME_INDEX: &[u8] = b"[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\nDirectories=scalable/apps\n\n          [scalable/apps]\nSize=48\nMinSize=8\nMaxSize=512\nType=Scalable\nContext=Applications\n";

pub(crate) fn uninstall() -> Result<(), String> {
    remove_old_theme_index()?;
    for path in [entry_path()?, icon_path()?, autostart_path()?] {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("not installed: {}", path.display());
            }
            Err(e) => return Err(format!("cannot remove {}: {e}", path.display())),
        }
    }
    refresh_caches();
    Ok(())
}

/// The release binary, which must exist: an entry pointing at a path
/// with nothing on it is a menu item that does nothing, and the reader
/// would have no way to tell that from a crash on startup.
fn built_binary() -> Result<PathBuf, String> {
    let path = repo_root().join("target/release/mattermost-rs");
    if !path.is_file() {
        return Err(format!(
            "{} does not exist -- run `make build` first, since the entry has to \
             point at a binary that is actually there",
            path.display()
        ));
    }
    // Absolute, because the desktop runs it from somewhere else entirely.
    path.canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

fn data_home() -> Result<PathBuf, String> {
    if let Some(set) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(set);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share"))
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_owned())
}

fn icon_path() -> Result<PathBuf, String> {
    Ok(data_home()?
        .join(ICON_DIR)
        .join(format!("{id}.svg", id = mm_desktop::APP_ID)))
}

fn entry_path() -> Result<PathBuf, String> {
    Ok(data_home()?
        .join(ENTRY_DIR)
        .join(format!("{id}.desktop", id = mm_desktop::APP_ID)))
}

/// Where the desktop looks for things to start at login.
///
/// Under config rather than data: it is a choice the reader made, not
/// a file the application needs to run.
fn autostart_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_owned())?;
    Ok(base
        .join("autostart")
        .join(format!("{id}.desktop", id = mm_desktop::APP_ID)))
}

fn repo_root() -> PathBuf {
    // `xtask` lives one directory down from the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Tell the desktop to re-read what changed.
///
/// Best effort on purpose: both tools are optional, and a desktop that
/// has neither simply picks the change up on the next login. Failing
/// the install over a missing cache updater would be worse than the
/// short wait.
fn refresh_caches() {
    let Ok(data) = data_home() else { return };
    // Only the applications directory, which is ours to speak for.
    //
    // There used to be a `gtk-update-icon-cache` on
    // `~/.local/share/icons/hicolor` here as well, and it is gone for the
    // same reason the `index.theme` it went with is gone: that tree is
    // the shared per-user fallback theme, and a cache written into it
    // describes *everybody's* icons, not just ours.
    let tools: [(&str, Vec<PathBuf>); 1] =
        [("update-desktop-database", vec![data.join(ENTRY_DIR)])];
    for (tool, args) in tools {
        let ran = Command::new(tool).args(&args).status();
        match ran {
            Ok(status) if status.success() => println!("ran {tool}"),
            Ok(status) => println!(
                "{tool} exited with {status} -- harmless, the \
                                    desktop will catch up on the next login"
            ),
            Err(_) => println!(
                "{tool} is not installed -- harmless, the desktop \
                                will catch up on the next login"
            ),
        }
    }
}

/// Take back the `index.theme` an older version wrote.
///
/// It declared the shared per-user fallback theme to contain one
/// directory -- ours -- and GTK takes the first `index.theme` it finds
/// as *the* definition of a theme rather than merging it with the
/// system's. Every icon in every other directory of that tree stopped
/// resolving: on the machine where this was found, Steam, Telegram, a
/// docker tool and a row of `JetBrains` applications lost their icons,
/// and so did parts of GNOME's own interface.
///
/// Only when the file is byte-for-byte what that version wrote. A
/// hicolor index is shared ground, and an uninstaller that guesses at
/// which files are its own does the same kind of damage in reverse.
fn remove_old_theme_index() -> Result<(), String> {
    let path = data_home()?.join("icons/hicolor/index.theme");
    let Ok(found) = std::fs::read(&path) else {
        return Ok(());
    };
    if found != OLD_THEME_INDEX {
        return Ok(());
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    println!(
        "removed {} (an older version wrote it, and it hid other applications' icons)",
        path.display()
    );
    Ok(())
}
