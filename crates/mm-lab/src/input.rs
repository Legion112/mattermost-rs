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
use x11rb::wrapper::ConnectionExt as _;

use crate::{Error, Result};

/// X keysyms the scenarios use, by name rather than by number.
pub mod key {
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const UP: u32 = 0xff52;
    pub const DOWN: u32 = 0xff54;
    pub const BACKSLASH: u32 = 0x5c;
    pub const CONTROL: u32 = 0xffe3;
    pub const ALT: u32 = 0xffe9;
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
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    // Let the server deliver before the connection goes away.
    std::thread::sleep(std::time::Duration::from_millis(20));
    Ok(())
}

/// Click the left button somewhere on the screen.
pub fn click_at(x: i32, y: i32) -> Result<()> {
    click_with(1, x, y)
}

/// The right button, for a context menu.
pub fn right_click_at(x: i32, y: i32) -> Result<()> {
    click_with(3, x, y)
}

fn click_with(button: u8, x: i32, y: i32) -> Result<()> {
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
    fake(xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    std::thread::sleep(std::time::Duration::from_millis(12));
    fake(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
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

/// Where a window of exactly this size sits on the screen.
///
/// A widget's extents are relative to *its* toplevel, and a dialog or
/// the quick switcher is a toplevel of its own, smaller than the main
/// window and somewhere else: adding the main window's place to a
/// widget in the switcher put the first clicks into the sidebar
/// behind it. The toplevel's size is what AT-SPI does say, so the X
/// window with that size is the one.
pub fn origin_of_window_sized(width: i32, height: i32) -> Result<(i32, i32)> {
    let (conn, root) = connect()?;
    let children = conn
        .query_tree(root)
        .map_err(|e| Error(format!("asking for the windows: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading the windows: {e}")))?
        .children;
    // The last match: a window mapped later sits above an older one of
    // the same size, and is the one the pointer would reach.
    let mut found = None;
    for child in children {
        let Ok(geometry) = conn.get_geometry(child).and_then(|c| {
            c.reply()
                .map_err(|_| x11rb::errors::ConnectionError::UnknownError)
        }) else {
            continue;
        };
        if i32::from(geometry.width) == width && i32::from(geometry.height) == height {
            found = Some((i32::from(geometry.x), i32::from(geometry.y)));
        }
    }
    found.map_or_else(toplevel_origin, Ok)
}

/// Turn the wheel over a spot: `ticks` notches, up or down. Buttons 4
/// and 5 are what a wheel is to X.
pub fn scroll_at(x: i32, y: i32, up: bool, ticks: u32) -> Result<()> {
    let (conn, root) = connect()?;
    let (sx, sy) = (
        i16::try_from(x).unwrap_or(i16::MAX),
        i16::try_from(y).unwrap_or(i16::MAX),
    );
    conn.xtest_fake_input(
        xproto::MOTION_NOTIFY_EVENT,
        0,
        x11rb::CURRENT_TIME,
        root,
        sx,
        sy,
        0,
    )
    .map_err(|e| Error(format!("moving the pointer: {e}")))?;
    let button = if up { 4 } else { 5 };
    for _ in 0..ticks {
        for type_ in [xproto::BUTTON_PRESS_EVENT, xproto::BUTTON_RELEASE_EVENT] {
            conn.xtest_fake_input(type_, button, x11rb::CURRENT_TIME, root, 0, 0, 0)
                .map_err(|e| Error(format!("turning the wheel: {e}")))?;
        }
        conn.flush()
            .map_err(|e| Error(format!("flushing the display: {e}")))?;
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    std::thread::sleep(std::time::Duration::from_millis(60));
    Ok(())
}

/// What the clipboard holds, as text.
///
/// Asked the X way: a window of this process asks the selection's owner
/// -- the client under test, which GTK makes the owner when it copies
/// -- to convert `CLIPBOARD` to `UTF8_STRING` into a property on that
/// window, and reads the property when the owner says it is there. A
/// clipboard with nothing in it, or nothing textual, answers `None`.
pub fn clipboard_text() -> Result<Option<String>> {
    let (conn, root) = connect()?;
    let atom = |name: &str| -> Result<xproto::Atom> {
        conn.intern_atom(false, name.as_bytes())
            .map_err(|e| Error(format!("naming {name}: {e}")))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| Error(format!("naming {name}: {e}")))
    };
    let (clipboard, utf8, slot) = (
        atom("CLIPBOARD")?,
        atom("UTF8_STRING")?,
        atom("MMRS_PASTE")?,
    );
    let window = conn
        .generate_id()
        .map_err(|e| Error(format!("a window id for the clipboard: {e}")))?;
    let screen = &conn.setup().roots[0];
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &xproto::CreateWindowAux::new().event_mask(xproto::EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| Error(format!("a window for the clipboard: {e}")))?;
    conn.convert_selection(window, clipboard, utf8, slot, x11rb::CURRENT_TIME)
        .map_err(|e| Error(format!("asking for the clipboard: {e}")))?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let answer = loop {
        if std::time::Instant::now() > deadline {
            return Err(Error("nobody answered for the clipboard".to_owned()));
        }
        match conn
            .poll_for_event()
            .map_err(|e| Error(format!("waiting for the clipboard: {e}")))?
        {
            Some(x11rb::protocol::Event::SelectionNotify(notify)) => break notify,
            Some(_) => {}
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    if answer.property == x11rb::NONE {
        return Ok(None);
    }
    let held = conn
        .get_property(true, window, slot, utf8, 0, u32::MAX)
        .map_err(|e| Error(format!("reading the clipboard: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading the clipboard: {e}")))?;
    let _ = conn.destroy_window(window);
    let _ = conn.flush();
    if held.type_ != utf8 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&held.value).into_owned()))
}

/// A left click with a key held: Alt+click, which marks a message unread.
///
/// The key goes down on a connection of its own before the click and
/// comes up after it; the server keeps the key state, so the click's
/// connection sees the modifier.
pub fn click_holding(keysym: u32, x: i32, y: i32) -> Result<()> {
    let (conn, _) = connect()?;
    let code = keycode_for(&conn, keysym)?;
    let fake = |type_: u8| {
        conn.xtest_fake_input(type_, code, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0)
            .map_err(|e| Error(format!("holding a key: {e}")))
            .map(|_| ())
    };
    fake(xproto::KEY_PRESS_EVENT)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    let clicked = click_with(1, x, y);
    fake(xproto::KEY_RELEASE_EVENT)?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    clicked
}

/// Put text on the clipboard and answer for it until `held` has passed.
///
/// The X way round: a window of this process takes `CLIPBOARD` and a
/// thread answers every `SelectionRequest` for it -- the list of
/// targets, and the text as `UTF8_STRING` or `STRING` -- the way the
/// client under test does when it has copied. The thread stops when
/// the time is up or something else takes the selection.
pub fn offer_clipboard_text(text: &str, held: std::time::Duration) -> Result<()> {
    let (conn, root) = connect()?;
    let atom = |name: &str| -> Result<xproto::Atom> {
        conn.intern_atom(false, name.as_bytes())
            .map_err(|e| Error(format!("naming {name}: {e}")))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| Error(format!("naming {name}: {e}")))
    };
    let (clipboard, utf8, targets, string) = (
        atom("CLIPBOARD")?,
        atom("UTF8_STRING")?,
        atom("TARGETS")?,
        atom("STRING")?,
    );
    let window = conn
        .generate_id()
        .map_err(|e| Error(format!("a window id for the clipboard: {e}")))?;
    let visual = conn.setup().roots[0].root_visual;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        visual,
        &xproto::CreateWindowAux::new(),
    )
    .map_err(|e| Error(format!("a window for the clipboard: {e}")))?;
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .map_err(|e| Error(format!("taking the clipboard: {e}")))?;
    conn.flush()
        .map_err(|e| Error(format!("flushing the display: {e}")))?;
    let owner = conn
        .get_selection_owner(clipboard)
        .map_err(|e| Error(format!("asking who holds the clipboard: {e}")))?
        .reply()
        .map_err(|e| Error(format!("asking who holds the clipboard: {e}")))?
        .owner;
    if owner != window {
        return Err(Error("the clipboard could not be taken".to_owned()));
    }
    let text = text.as_bytes().to_vec();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + held;
        while std::time::Instant::now() < deadline {
            let event = match conn.poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            match event {
                x11rb::protocol::Event::SelectionRequest(request) => {
                    answer_for_clipboard(&conn, &request, &text, (targets, utf8, string));
                }
                x11rb::protocol::Event::SelectionClear(_) => return,
                _ => {}
            }
        }
        let _ = conn.destroy_window(window);
        let _ = conn.flush();
    });
    Ok(())
}

/// Answer one request for the clipboard: the targets on offer, or the
/// text itself, written to the requestor's property and announced with
/// a `SelectionNotify` -- with no property when the target is not one
/// this owner has.
fn answer_for_clipboard(
    conn: &x11rb::rust_connection::RustConnection,
    request: &xproto::SelectionRequestEvent,
    text: &[u8],
    (targets, utf8, string): (xproto::Atom, xproto::Atom, xproto::Atom),
) {
    let property = if request.property == x11rb::NONE {
        request.target
    } else {
        request.property
    };
    let answered = if request.target == targets {
        conn.change_property32(
            xproto::PropMode::REPLACE,
            request.requestor,
            property,
            xproto::AtomEnum::ATOM,
            &[targets, utf8, string],
        )
        .is_ok()
    } else if request.target == utf8 || request.target == string {
        conn.change_property8(
            xproto::PropMode::REPLACE,
            request.requestor,
            property,
            request.target,
            text,
        )
        .is_ok()
    } else {
        false
    };
    let notify = xproto::SelectionNotifyEvent {
        response_type: xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: if answered { property } else { x11rb::NONE },
    };
    let _ = conn.send_event(
        false,
        request.requestor,
        xproto::EventMask::NO_EVENT,
        notify,
    );
    let _ = conn.flush();
}
