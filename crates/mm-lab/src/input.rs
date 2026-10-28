//! Keys and clicks put into the lab's display, as a person would.
//!
//! AT-SPI drives what a widget *offers* -- a button's one action, an
//! entry's text -- and that is most of a window. It cannot press Return
//! in a box, hold a chord, click into the middle of a paragraph or rest
//! the pointer on something. The display under the lab is Xvfb, which
//! speaks XTEST, and XTEST puts real input events on the server: the
//! window sees a key or a button the way it sees them from a keyboard.
//! Only ever the lab's own display, which is the whole of the container.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;

use crate::{Error, Result};

/// X keysyms the scenarios use, by name rather than by number.
pub mod key {
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const UP: u32 = 0xff52;
    pub const BACKSLASH: u32 = 0x5c;
    pub const CONTROL: u32 = 0xffe3;
    pub const SHIFT: u32 = 0xffe1;
}

fn connect() -> Result<(x11rb::rust_connection::RustConnection, xproto::Window)> {
    let (conn, screen) =
        x11rb::connect(None).map_err(|e| Error(format!("no X display to type into: {e}")))?;
    let root = conn.setup().roots[screen].root;
    // What xdotool asks for first: to go on generating input while
    // another client holds the server.
    conn.xtest_grab_control(true)
        .map_err(|e| Error(format!("asking for grab control: {e}")))?
        .check()
        .map_err(|e| Error(format!("the display refused grab control: {e}")))?;
    Ok((conn, root))
}

/// The keycode that produces a keysym on this display's keyboard.
fn keycode_for(conn: &x11rb::rust_connection::RustConnection, keysym: u32) -> Result<u8> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| Error(format!("asking for the keyboard map: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading the keyboard map: {e}")))?;
    let per = usize::from(mapping.keysyms_per_keycode.max(1));
    mapping
        .keysyms
        .chunks(per)
        .position(|syms| syms.contains(&keysym))
        .and_then(|at| u8::try_from(at).ok())
        .map(|at| min + at)
        .ok_or_else(|| {
            Error(format!(
                "no key on this keyboard produces keysym {keysym:#x}"
            ))
        })
}

/// Hand the keyboard to the toplevel under the pointer.
///
/// There is no window manager on the lab's display, so nothing ever
/// does this otherwise. GTK wants a proper `FocusIn` on the toplevel
/// before a click moves its focus widget or a key reaches the one
/// that has the caret: without it a click put the caret in the box
/// and Return still went to the button pressed last.
fn focus_under_pointer(
    conn: &x11rb::rust_connection::RustConnection,
    root: xproto::Window,
) -> Result<xproto::Window> {
    let under = conn
        .query_pointer(root)
        .map_err(|e| Error(format!("asking what is under the pointer: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading what is under the pointer: {e}")))?
        .child;
    let focus = if under == x11rb::NONE { root } else { under };
    conn.set_input_focus(xproto::InputFocus::PARENT, focus, x11rb::CURRENT_TIME)
        .map_err(|e| Error(format!("giving the window the keyboard: {e}")))?
        .check()
        .map_err(|e| Error(format!("the display refused the focus: {e}")))?;
    Ok(focus)
}

/// Press a key, holding the modifiers, and let everything go again.
pub fn press(keysym: u32, modifiers: &[u32]) -> Result<()> {
    let (conn, root) = connect()?;
    let focus = focus_under_pointer(&conn, root)?;
    eprintln!("  key {keysym:#x} to window {focus:#x}");
    // `root` is `None` for a key, as libXtst sends it, and the request
    // is checked: it answers nothing, so an error in it -- a BadValue
    // for the window, say -- would otherwise be thrown away unseen,
    // which is how a key can be faked and never arrive.
    let fake = |type_: u8, code: u8| {
        conn.xtest_fake_input(type_, code, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0)
            .map_err(|e| Error(format!("putting a key on the display: {e}")))?
            .check()
            .map_err(|e| Error(format!("the display refused the key: {e}")))
    };
    let held: Vec<u8> = modifiers
        .iter()
        .map(|m| keycode_for(&conn, *m))
        .collect::<Result<_>>()?;
    let code = keycode_for(&conn, keysym)?;
    for m in &held {
        fake(xproto::KEY_PRESS_EVENT, *m)?;
    }
    fake(xproto::KEY_PRESS_EVENT, code)?;
    fake(xproto::KEY_RELEASE_EVENT, code)?;
    for m in held.iter().rev() {
        fake(xproto::KEY_RELEASE_EVENT, *m)?;
    }
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))
}

/// Put the pointer somewhere on the screen.
pub fn move_to(x: i32, y: i32) -> Result<()> {
    let (conn, root) = connect()?;
    let (x, y) = (
        i16::try_from(x).unwrap_or(i16::MAX),
        i16::try_from(y).unwrap_or(i16::MAX),
    );
    conn.xtest_fake_input(
        xproto::MOTION_NOTIFY_EVENT,
        0,
        x11rb::CURRENT_TIME,
        root,
        x,
        y,
        0,
    )
    .map_err(|e| Error(format!("moving the pointer: {e}")))?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))
}

/// Click the left button somewhere on the screen.
pub fn click_at(x: i32, y: i32) -> Result<()> {
    // One connection for the whole gesture, the way xdotool does it:
    // the move, the press, a finger's pause, the release, one flush.
    let (conn, root) = connect()?;
    let (sx, sy) = (
        i16::try_from(x).unwrap_or(i16::MAX),
        i16::try_from(y).unwrap_or(i16::MAX),
    );
    let fake = |type_: u8, detail: u8, rx: i16, ry: i16| {
        conn.xtest_fake_input(type_, detail, x11rb::CURRENT_TIME, root, rx, ry, 0)
            .map_err(|e| Error(format!("clicking: {e}")))
            .map(|_| ())
    };
    fake(xproto::MOTION_NOTIFY_EVENT, 0, sx, sy)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    // The window under the pointer gets the keyboard before the button
    // goes down: a click into an inactive window put the caret in the
    // box and GTK handed focus back to the last widget when the window
    // was next activated.
    focus_under_pointer(&conn, root)?;
    fake(xproto::BUTTON_PRESS_EVENT, 1, 0, 0)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    std::thread::sleep(std::time::Duration::from_millis(12));
    fake(xproto::BUTTON_RELEASE_EVENT, 1, 0, 0)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    // Let the server deliver before the connection goes away.
    std::thread::sleep(std::time::Duration::from_millis(30));
    Ok(())
}

/// Where the application's window sits on the screen.
///
/// AT-SPI places a widget inside its window and says nothing true about
/// the screen -- it answered (0, 0) for every widget -- so the window's
/// own place comes from X: the biggest window the root has, which with
/// no window manager is the toplevel itself, exactly where it was mapped.
pub fn toplevel_origin() -> Result<(i32, i32)> {
    let (conn, root) = connect()?;
    let children = conn
        .query_tree(root)
        .map_err(|e| Error(format!("asking for the windows: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading the windows: {e}")))?
        .children;
    let mut best: Option<(u32, i32, i32)> = None;
    for child in children {
        let Ok(geometry) = conn.get_geometry(child).and_then(|c| {
            c.reply()
                .map_err(|_| x11rb::errors::ConnectionError::UnknownError)
        }) else {
            continue;
        };
        let area = u32::from(geometry.width) * u32::from(geometry.height);
        if best.is_none_or(|(held, _, _)| area > held) {
            best = Some((area, i32::from(geometry.x), i32::from(geometry.y)));
        }
    }
    best.map(|(_, x, y)| (x, y))
        .ok_or_else(|| Error("no window on the display".to_owned()))
}
