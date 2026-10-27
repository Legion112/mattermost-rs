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
    write_theme_index()?;
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

pub(crate) fn uninstall() -> Result<(), String> {
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

/// Give `~/.local/share/icons/hicolor` an `index.theme` if it has none.
///
/// Without one the directory is not a theme, `gtk-update-icon-cache`
/// refuses it, and **nothing looks the icon up by name** -- which is
/// invisible until something asks for it by name rather than by path.
/// The tray does exactly that, so the icon registered fine and then
/// drew as nothing.
///
/// Never overwritten: a hicolor with an index already belongs to
/// something else, very likely the distribution.
fn write_theme_index() -> Result<(), String> {
    let path = data_home()?.join("icons/hicolor/index.theme");
    if path.exists() {
        return Ok(());
    }
    write(
        &path,
        b"[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\nDirectories=scalable/apps\n\n          [scalable/apps]\nSize=48\nMinSize=8\nMaxSize=512\nType=Scalable\nContext=Applications\n",
    )?;
    println!(
        "  {} (created: the directory was not a theme)",
        path.display()
    );
    Ok(())
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
    let tools: [(&str, Vec<PathBuf>); 2] = [
        ("update-desktop-database", vec![data.join(ENTRY_DIR)]),
        (
            "gtk-update-icon-cache",
            vec![PathBuf::from("-f"), data.join("icons/hicolor")],
        ),
    ];
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
