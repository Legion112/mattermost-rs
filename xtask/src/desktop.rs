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

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the icon and the entry go, relative to the data home.
const ICON_DIR: &str = "icons/hicolor/scalable/apps";
/// The same directory as `ICON_DIR`, named the way an `index.theme`
/// names it: relative to the theme rather than to the data directory.
const ICON_LEAF: &str = "scalable/apps";
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
/// **`Directories` is built from what is actually there, never from what
/// this application needs.** That distinction is the whole of this
/// function. An earlier version wrote `Directories=scalable/apps` and
/// nothing else, and `~/.local/share/icons/hicolor` is not ours: it is
/// the shared per-user fallback theme, and on a real desktop it already
/// held eleven directories of other applications' icons -- Steam,
/// Telegram, a pile of `JetBrains` tools. Naming one directory in the
/// index told every icon lookup that the other ten did not exist, and
/// those applications lost their icons the next time anything rebuilt
/// the cache. Writing a theme definition that is narrower than the
/// theme is how you break somebody else's desktop from an installer
/// that only meant to add one file.
///
/// Never overwritten either: an index already there belongs to
/// something else, very likely the distribution.
fn write_theme_index() -> Result<(), String> {
    let hicolor = data_home()?.join("icons/hicolor");
    let path = hicolor.join("index.theme");
    if path.exists() {
        return Ok(());
    }
    let found = icon_directories(&hicolor);
    let mut body = String::from("[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\n");
    let _ = writeln!(body, "Directories={}\n", found.join(","));
    for directory in &found {
        body.push_str(&stanza(directory));
    }
    write(&path, body.as_bytes())?;
    println!(
        "  {} (created: the directory was not a theme; {} directories listed)",
        path.display(),
        found.len()
    );
    Ok(())
}

/// Every `<size>/<context>` directory under a hicolor tree, plus the one
/// this installer is about to write into.
///
/// Sorted and deduplicated so the file is stable from run to run, which
/// matters only because a file that changes for no reason invites
/// somebody to wonder what changed.
fn icon_directories(hicolor: &Path) -> Vec<String> {
    let mut found = vec![ICON_LEAF.to_owned()];
    if let Ok(sizes) = std::fs::read_dir(hicolor) {
        for size in sizes.filter_map(Result::ok) {
            if !size.path().is_dir() {
                continue;
            }
            let Ok(contexts) = std::fs::read_dir(size.path()) else {
                continue;
            };
            for context in contexts.filter_map(Result::ok) {
                if !context.path().is_dir() {
                    continue;
                }
                if let (Some(a), Some(b)) = (
                    size.file_name().to_str().map(str::to_owned),
                    context.file_name().to_str().map(str::to_owned),
                ) {
                    found.push(format!("{a}/{b}"));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The `[<size>/<context>]` section for one directory.
///
/// `scalable` and `symbolic` hold SVGs and are typed accordingly;
/// everything else is named after the pixel size it holds, and anything
/// whose name does not start with a number is treated as scalable rather
/// than guessed at.
fn stanza(directory: &str) -> String {
    let (size, context) = directory.split_once('/').unwrap_or((directory, "apps"));
    let context = match context {
        "apps" => "Applications",
        other => other,
    };
    let pixels: Option<u32> = size.split(['x', '@']).next().and_then(|n| n.parse().ok());
    if let Some(n) = pixels {
        return format!("[{directory}]\nSize={n}\nType=Threshold\nContext={context}\n\n");
    }
    // A symbolic icon is nominally 16px even though it scales, which is
    // what every other theme on the system says about it.
    let nominal = if size == "symbolic" { 16 } else { 48 };
    format!(
        "[{directory}]\nSize={nominal}\nMinSize=8\nMaxSize=512\nType=Scalable\n\
         Context={context}\n\n"
    )
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
