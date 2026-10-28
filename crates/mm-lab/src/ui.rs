//! Driving the client's own widgets, over the accessibility bus.
//!
//! The lab could bring a server up and fill it, but every check on what the
//! *window* did was a screenshot read by eye. That is not a test: it cannot
//! fail in CI, it cannot run twice the same way, and it proves nothing the
//! next change cannot quietly break.
//!
//! This drives the real widgets instead. GTK4 publishes its whole widget
//! tree on the AT-SPI bus -- the same tree a screen reader uses -- with a
//! role and a name for every widget and an `Action` interface that activates
//! one. So a test can say "press the button called Leave channel" and mean
//! the button, not a pair of screen coordinates that the next layout change
//! invalidates. Nothing here synthesises an X event, so it needs no `unsafe`,
//! no X11, and nothing outside this repository.
//!
//! What it does *not* do is prove anything about how the window looks. A
//! widget can be correct in the tree and invisible on screen. Taking a
//! screenshot is still the way to answer that, and still a human's job.

use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use crate::{Error, Result};

const ROOT: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ACTION: &str = "org.a11y.atspi.Action";
const EDITABLE: &str = "org.a11y.atspi.EditableText";
const SELECTION: &str = "org.a11y.atspi.Selection";
const COMPONENT: &str = "org.a11y.atspi.Component";
const TEXT: &str = "org.a11y.atspi.Text";

/// How far outside the window to photograph, so a popover that hangs over
/// the edge is in the picture.
const AROUND: i32 = 80;

/// `AtspiStateType::Sensitive`. The states arrive as a two-word bitfield and
/// the index is the enum's own position, which is not written down anywhere
/// a compiler can check -- so it is asserted against a widget known to be
/// insensitive rather than trusted.
const STATE_SENSITIVE: u32 = 24;

/// How deep to walk before deciding the tree is a loop. A GTK window is a
/// dozen levels; sixty is far past anything real.
const MAX_DEPTH: usize = 60;

/// One widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub bus: String,
    pub path: OwnedObjectPath,
    pub role: String,
    pub name: String,
    pub depth: usize,
}

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:indent$}{} {:?}",
            "",
            self.role,
            self.name,
            indent = self.depth * 2
        )
    }
}

/// A window of the client, as the accessibility bus sees it.
#[derive(Debug)]
pub struct Screen {
    conn: Connection,
    app: String,
}

impl Screen {
    /// Attach to the client whose window title contains `marker`.
    ///
    /// By the window's title and not the application's name, because both
    /// the lab instance and the one somebody has open all day are called
    /// `mattermost-rs` on the bus. `MMRS_APP_ID` is what puts the bracketed
    /// marker in the title, and this is the second thing that depends on it.
    pub async fn attach(marker: &str) -> Result<Self> {
        let session = Connection::session()
            .await
            .map_err(|e| Error(format!("no session bus: {e}")))?;
        let address: String = zbus::Proxy::new(
            &session,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
        )
        .await
        .map_err(|e| Error(format!("no accessibility bus: {e}")))?
        .call("GetAddress", &())
        .await
        .map_err(|e| {
            Error(format!(
                "the accessibility bus did not give an address: {e}\nIt is what GTK publishes \
                 the widget tree on; without it there is nothing to drive."
            ))
        })?;

        let conn = zbus::connection::Builder::address(address.as_str())
            .map_err(|e| Error(format!("bad accessibility bus address: {e}")))?
            .build()
            .await
            .map_err(|e| Error(format!("cannot reach the accessibility bus: {e}")))?;

        let registry = Self {
            conn,
            app: "org.a11y.atspi.Registry".to_owned(),
        };
        let root = Node {
            bus: registry.app.clone(),
            path: OwnedObjectPath::try_from(ROOT).map_err(|e| Error(e.to_string()))?,
            role: "registry".to_owned(),
            name: String::new(),
            depth: 0,
        };

        let mut tried = Vec::new();
        for (bus, _) in registry.children(&root).await? {
            let app = Self {
                conn: registry.conn.clone(),
                app: bus.clone(),
            };
            let top = Node {
                bus: bus.clone(),
                path: root.path.clone(),
                role: "application".to_owned(),
                name: String::new(),
                depth: 0,
            };
            let Ok(name) = app.name_of(&top).await else {
                continue;
            };
            if name != "mattermost-rs" {
                continue;
            }
            // Its windows, which is where the marker is.
            for (_, path) in app.children(&top).await.unwrap_or_default() {
                let frame = Node {
                    bus: bus.clone(),
                    path,
                    role: "frame".to_owned(),
                    name: String::new(),
                    depth: 0,
                };
                let title = app.name_of(&frame).await.unwrap_or_default();
                tried.push(title.clone());
                if title.contains(marker) {
                    return Ok(app);
                }
            }
        }
        Err(Error(format!(
            "no mattermost-rs window whose title contains {marker:?}. Windows seen: {tried:?}\n\
             Start one with `mmlab run`, which sets MMRS_APP_ID."
        )))
    }

    /// Attach once the window exists, rather than failing because the
    /// client is still starting.
    pub async fn attach_waiting(marker: &str) -> Result<Self> {
        let deadline = std::time::Instant::now() + PATIENCE;
        let mut last = Error("never tried".to_owned());
        while std::time::Instant::now() < deadline {
            match Self::attach(marker).await {
                Ok(screen) => return Ok(screen),
                Err(e) => last = e,
            }
            tokio::time::sleep(POLL).await;
        }
        Err(last)
    }

    async fn proxy<'a>(&self, node: &Node, interface: &'a str) -> Result<zbus::Proxy<'a>> {
        zbus::Proxy::new(
            &self.conn,
            self.app.clone(),
            node.path.clone(),
            interface.to_owned(),
        )
        .await
        .map_err(|e| Error(format!("{} {}: {e}", node.role, node.name)))
    }

    async fn name_of(&self, node: &Node) -> Result<String> {
        match self
            .proxy(node, ACCESSIBLE)
            .await?
            .get_property("Name")
            .await
        {
            Ok(name) => Ok(name),
            Err(e) if gone(&e) => Ok(String::new()),
            Err(e) => Err(Error(format!("reading a name: {e}"))),
        }
    }

    async fn children(&self, node: &Node) -> Result<Vec<(String, OwnedObjectPath)>> {
        match self
            .proxy(node, ACCESSIBLE)
            .await?
            .call("GetChildren", &())
            .await
        {
            Ok(children) => Ok(children),
            // The widget left the tree between being listed and being
            // read. A live window does that: a channel's list grows in
            // steps after it opens and lets go of rows as it moves, and
            // a walk that happens to be passing through one of them is
            // not a failure of the thing being tested. It has no
            // children any more, which is the truth.
            Err(e) if gone(&e) => Ok(Vec::new()),
            Err(e) => Err(Error(format!("reading children of {}: {e}", node.name))),
        }
    }

    /// Every widget under the window, depth first, in the order they are laid
    /// out. The order matters: "the first Add button after the row called
    /// @linus" is how a test picks one of several identical buttons.
    pub async fn tree(&self) -> Result<Vec<Node>> {
        let top = Node {
            bus: self.app.clone(),
            path: OwnedObjectPath::try_from(ROOT).map_err(|e| Error(e.to_string()))?,
            role: "application".to_owned(),
            name: String::new(),
            depth: 0,
        };
        let mut out = Vec::new();
        self.walk(&top, &mut out).await?;
        Ok(out)
    }

    /// Recursion by hand rather than an `async fn` calling itself, which
    /// needs a box at every level. A stack, children pushed in reverse, so
    /// what comes out is depth-first in layout order.
    async fn walk(&self, from: &Node, out: &mut Vec<Node>) -> Result<()> {
        let mut stack = self.describe_children(from).await?;
        stack.reverse();
        while let Some(node) = stack.pop() {
            if node.depth > MAX_DEPTH {
                continue;
            }
            let mut kids = self.describe_children(&node).await.unwrap_or_default();
            out.push(node);
            kids.reverse();
            stack.extend(kids);
        }
        Ok(())
    }

    /// A widget's children, each with its role and name already read.
    async fn describe_children(&self, parent: &Node) -> Result<Vec<Node>> {
        let mut out = Vec::new();
        for (_, path) in self.children(parent).await? {
            let mut child = Node {
                bus: self.app.clone(),
                path,
                role: String::new(),
                name: String::new(),
                depth: parent.depth + 1,
            };
            child.role = self.role_of(&child).await.unwrap_or_default();
            child.name = self.name_of(&child).await.unwrap_or_default();
            out.push(child);
        }
        Ok(out)
    }

    async fn role_of(&self, node: &Node) -> Result<String> {
        self.proxy(node, ACCESSIBLE)
            .await?
            .call("GetRoleName", &())
            .await
            .map_err(|e| Error(format!("reading a role: {e}")))
    }

    /// Which AT-SPI interfaces a widget offers, which is what says whether
    /// it can be pressed, typed into, or selected within.
    pub async fn interfaces(&self, node: &Node) -> Vec<String> {
        let Ok(proxy) = self.proxy(node, ACCESSIBLE).await else {
            return Vec::new();
        };
        proxy
            .call::<_, _, Vec<String>>("GetInterfaces", &())
            .await
            .unwrap_or_default()
    }

    /// What a widget says it can be asked to do. Empty means it is not a
    /// control, whatever it looks like.
    pub async fn actions(&self, node: &Node) -> Vec<String> {
        let Ok(proxy) = self.proxy(node, ACTION).await else {
            return Vec::new();
        };
        let Ok(count): std::result::Result<i32, _> = proxy.get_property("NActions").await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..count {
            if let Ok(name) = proxy.call::<_, _, String>("GetName", &(i)).await {
                out.push(name);
            }
        }
        out
    }

    /// Activate a widget: the same thing a screen reader's "press" does.
    pub async fn click(&self, node: &Node) -> Result<()> {
        let done: bool = self
            .proxy(node, ACTION)
            .await?
            .call("DoAction", &(0_i32))
            .await
            .map_err(|e| Error(format!("pressing {:?}: {e}", node.name)))?;
        if !done {
            return Err(Error(format!("{:?} refused to be pressed", node.name)));
        }
        Ok(())
    }

    /// Put text into an entry, as if it had been typed.
    pub async fn type_into(&self, node: &Node, text: &str) -> Result<()> {
        // No `Component.GrabFocus` first: GTK4 answers it `NotSupported`,
        // and `SetTextContents` does not need the focus anyway -- it is the
        // same call a screen reader's braille keyboard makes.
        let done: bool = self
            .proxy(node, EDITABLE)
            .await?
            .call("SetTextContents", &(text))
            .await
            .map_err(|e| Error(format!("typing into {:?}: {e}", node.name)))?;
        if !done {
            return Err(Error(format!("{:?} would not take text", node.name)));
        }
        Ok(())
    }

    /// The raw state bitfield, for working out which bit means what.
    pub async fn states(&self, node: &Node) -> Vec<u32> {
        let Ok(proxy) = self.proxy(node, ACCESSIBLE).await else {
            return Vec::new();
        };
        proxy
            .call::<_, _, Vec<u32>>("GetState", &())
            .await
            .unwrap_or_default()
    }

    /// Whether a widget can be used, which is what a greyed-out button is not.
    pub async fn sensitive(&self, node: &Node) -> Result<bool> {
        let states: Vec<u32> = self
            .proxy(node, ACCESSIBLE)
            .await?
            .call("GetState", &())
            .await
            .map_err(|e| Error(format!("reading the state of {:?}: {e}", node.name)))?;
        Ok(states
            .first()
            .is_some_and(|word| word & (1 << STATE_SENSITIVE) != 0))
    }
}

/// How long a test waits for the window to catch up with a click.
///
/// Every button here sends a command to the engine, which makes a request
/// and answers with an event the window redraws from -- so nothing is true
/// the instant it is pressed. Polling the tree rather than sleeping a fixed
/// time: the wait is then as short as the server is fast, and a failure says
/// what the tree held instead of what it should have.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(15);
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

impl Screen {
    /// Open a channel by the name on its sidebar row.
    ///
    /// Through the list's `Selection` interface rather than a click,
    /// because a `GtkListBoxRow` offers no action -- selecting a row *is*
    /// how a list box is used, and it is the same signal a mouse would
    /// raise.
    pub async fn open_channel(&self, label: &str) -> Result<()> {
        self.select_row(label).await?;
        // The header button's name is the channel's name *and* its
        // subtitle -- "Engineering 4 members" -- so this is a prefix
        // rather than an equality, and that subtitle is itself worth
        // asserting on later.
        self.until(&format!("{label} is open"), |tree| {
            tree.iter()
                .any(|n| n.role == "button" && n.name.starts_with(label))
        })
        .await
        .map(|_| ())
    }

    /// Select the list row carrying `label`, the way a click does, and
    /// wait for nothing: what selecting it opens is the caller's to
    /// check.
    ///
    /// Selected and then activated, because that is what a click is.
    /// Selecting a row that is already selected tells the window
    /// nothing at all, and a reader who clicks the highlighted channel
    /// to come back from Threads is doing exactly that.
    async fn select_row_once(&self, label: &str) -> Result<()> {
        let tree = self.tree().await?;
        // Every list, not the first one: an open popover puts its own list
        // of members in the tree ahead of the sidebar, and "the first list"
        // then means the roster of whatever menu was left open.
        let mut seen = Vec::new();
        for list in tree.iter().filter(|n| n.role == "list") {
            let rows = self.describe_children(list).await?;
            for (index, row) in rows.iter().enumerate() {
                let mut inside = Vec::new();
                self.walk(row, &mut inside).await?;
                if !inside.iter().any(|n| n.role == "label" && n.name == label) {
                    seen.extend(
                        inside
                            .iter()
                            .filter(|n| n.role == "label")
                            .map(|n| n.name.clone()),
                    );
                    continue;
                }
                let index = i32::try_from(index).unwrap_or(0);
                // Two halves of a click. A list that selects -- the
                // sidebar -- answers the first; a list that does not,
                // like the Threads view's, answers only the second; and a
                // row that is selected already answers only the second
                // too, since selecting it again is nothing.
                let selected: bool = self
                    .proxy(list, SELECTION)
                    .await?
                    .call("SelectChild", &(index))
                    .await
                    .unwrap_or(false);
                let activated = match self.proxy(row, ACTION).await {
                    Ok(action) => action
                        .call::<_, _, bool>("DoAction", &(0_i32))
                        .await
                        .unwrap_or(false),
                    Err(_) => false,
                };
                if !selected && !activated {
                    return Err(Error(format!(
                        "the list would neither select nor activate {label:?}"
                    )));
                }
                return Ok(());
            }
        }
        seen.sort();
        seen.dedup();
        Err(Error(format!(
            "no list row labelled {label:?}. Rows carried: {seen:?}"
        )))
    }

    /// `select_row_once`, tried again for a while when the row is not
    /// there: a list being rebuilt -- the sidebar after a `user_updated`,
    /// say -- has no rows for a moment, and one snapshot taken then is
    /// not the window being wrong.
    pub async fn select_row(&self, label: &str) -> Result<()> {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match self.select_row_once(label).await {
                Err(e)
                    if std::time::Instant::now() < deadline
                        && e.0.starts_with("no list row labelled") =>
                {
                    tokio::time::sleep(POLL).await;
                }
                other => return other,
            }
        }
    }

    /// Where a widget is on the screen: x, y, width, height.
    ///
    /// Asked for *in its window* (coordinate type 1) and moved by where
    /// the window is: GTK answers the screen question (type 0) with
    /// (0, 0) for every widget, which put the first clicks on the title
    /// bar.
    pub async fn extents(&self, node: &Node) -> Result<(i32, i32, i32, i32)> {
        let ((x, y, w, h),) = self
            .proxy(node, COMPONENT)
            .await?
            .call::<_, _, ((i32, i32, i32, i32),)>("GetExtents", &(1_u32))
            .await
            .map_err(|e| Error(format!("placing {:?}: {e}", node.name)))?;
        let (wx, wy) = crate::input::toplevel_origin()?;
        Ok((wx + x, wy + y, w, h))
    }

    /// What a text widget holds, start to end.
    pub async fn text_of(&self, node: &Node) -> Result<String> {
        self.proxy(node, TEXT)
            .await?
            .call("GetText", &(0_i32, -1_i32))
            .await
            .map_err(|e| Error(format!("reading {:?}: {e}", node.name)))
    }

    /// A real click, in the middle of a widget: what AT-SPI's one action
    /// cannot do for a text view, which has none.
    pub async fn click_into(&self, node: &Node) -> Result<()> {
        let (x, y, w, h) = self.extents(node).await?;
        // In the log, because a click that lands somewhere else is the
        // kind of failure the tree afterwards does not explain.
        eprintln!("  click into {:?} at ({x}, {y}) {w}x{h}", node.name);
        crate::input::click_at(x + w / 2, y + h / 2)
    }

    /// Click just inside a widget's top-left corner: the first word of a
    /// label, which a label cannot place any more finely -- GTK answers
    /// `GetRangeExtents` on one with `NotSupported`.
    pub async fn click_start_of(&self, node: &Node) -> Result<()> {
        let (x, y, _, _) = self.extents(node).await?;
        eprintln!("  click at the start of {:?} at ({x}, {y})", node.name);
        crate::input::click_at(x + 14, y + 10)
    }

    /// Click a button by its name with the pointer, for the ones that
    /// offer no AT-SPI action -- a `MenuButton`, which opens a popover,
    /// answers "No action with index 0".
    pub async fn click_button(&self, name: &str) -> Result<()> {
        let tree = self
            .until(&format!("a button called {name:?} is on screen"), |tree| {
                tree.has("button", name)
            })
            .await?;
        let button = tree
            .iter()
            .find(|n| n.role == "button" && n.name == name)
            .or_else(|| tree.find("button", name).ok())
            .ok_or_else(|| Error(format!("no button called {name:?}")))?;
        self.click_into(button).await
    }

    /// Click the button of this name nearest to another widget in layout
    /// order -- the hover toolbar's "More actions" beside *this* message,
    /// not the first such button on the page, which belongs to whichever
    /// row the pointer crossed earlier. A widget no size is a hidden one
    /// and is passed over.
    pub async fn click_button_near(&self, name: &str, anchor: &Node) -> Result<()> {
        let tree = self
            .until(&format!("a button called {name:?} is on screen"), |tree| {
                tree.has("button", name)
            })
            .await?;
        let at = tree.iter().position(|n| n.path == anchor.path).unwrap_or(0);
        let mut candidates: Vec<(usize, &Node)> = tree
            .iter()
            .enumerate()
            .filter(|(_, n)| n.role == "button" && n.name == name)
            .map(|(i, n)| (i.abs_diff(at), n))
            .collect();
        candidates.sort_by_key(|(distance, _)| *distance);
        for (_, button) in candidates {
            let (_, _, w, h) = self.extents(button).await?;
            if w > 0 && h > 0 {
                return self.click_into(button).await;
            }
        }
        Err(Error(format!("no visible button called {name:?}")))
    }

    /// Rest the pointer on a widget, for what shows on hover.
    pub async fn hover(&self, node: &Node) -> Result<()> {
        let (x, y, w, h) = self.extents(node).await?;
        // In two steps: GTK works out which widget the pointer entered
        // from one motion to the next, and a single warp from nowhere
        // onto the spot gave it nothing to compare.
        crate::input::move_to(x + w / 2 - 8, y + h / 2 - 4)?;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        crate::input::move_to(x + w / 2, y + h / 2)?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        Ok(())
    }

    /// Click on a run of text inside a text widget -- an `@name`, say.
    pub async fn click_text(&self, node: &Node, needle: &str) -> Result<()> {
        let text = self.text_of(node).await?;
        let at = text
            .find(needle)
            .ok_or_else(|| Error(format!("{needle:?} is not in {text:?}")))?;
        let start = i32::try_from(text[..at].chars().count()).unwrap_or(0);
        let end = start + i32::try_from(needle.chars().count()).unwrap_or(0);
        let ((x, y, w, h),) = self
            .proxy(node, TEXT)
            .await?
            .call::<_, _, ((i32, i32, i32, i32),)>("GetRangeExtents", &(start, end, 1_u32))
            .await
            .map_err(|e| Error(format!("placing {needle:?}: {e}")))?;
        let (wx, wy) = crate::input::toplevel_origin()?;
        eprintln!("  click on {needle:?} at ({x}, {y}) {w}x{h} in the window at ({wx}, {wy})");
        crate::input::click_at(wx + x + w / 2, wy + y + h / 2)
    }

    /// The first text widget holding this text.
    pub async fn text_holding(&self, needle: &str) -> Result<Node> {
        let tree = self
            .until(
                &format!("a text view holding {needle:?} is on screen"),
                |tree| {
                    tree.iter()
                        .any(|n| n.role == "text box" || n.role == "text")
                },
            )
            .await?;
        // Every widget with text in its role, and what each said, so a
        // miss names what was there rather than nothing.
        let deadline = std::time::Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            let tree = self.tree().await?;
            seen.clear();
            // And every role there is, since a text view that is not
            // a "text box" is the first thing to rule out.
            let mut roles: Vec<&str> = tree.iter().map(|n| n.role.as_str()).collect();
            roles.sort_unstable();
            roles.dedup();
            seen.push(format!("roles on screen: {roles:?}"));
            for label in tree
                .iter()
                .filter(|n| n.role == "label" && n.name.contains(needle))
            {
                seen.push(format!("a plain label says {:?}", label.name));
            }
            for node in tree
                .iter()
                .filter(|n| n.role.contains("text") || n.role == "label")
            {
                match self.text_of(node).await {
                    Ok(text) if text.contains(needle) => return Ok(node.clone()),
                    Ok(text) => seen.push(format!("{} {text:?}", node.role)),
                    Err(e) => seen.push(format!("{} (unreadable: {e})", node.role)),
                }
            }
            tokio::time::sleep(POLL).await;
        }
        let _ = tree;
        Err(Error(format!(
            "no text view holds {needle:?}. Text widgets seen: {seen:#?}"
        )))
    }

    /// Press a key on the display, with modifiers held: Return in a box,
    /// a chord, Escape on a popover.
    pub fn press_key(&self, keysym: u32, modifiers: &[u32]) -> Result<()> {
        crate::input::press(keysym, modifiers)
    }

    /// Where the window is on screen, so a failure photographs it rather
    /// than whatever else the developer had open.
    pub async fn window_area(&self) -> Option<crate::shot::Area> {
        let tree = self.tree().await.ok()?;
        // The first few nodes, not the first: which of them is the window
        // rather than a wrapper differs, and asking the wrong one gives
        // nothing or a zero-sized box.
        for node in tree.iter().take(6) {
            let Ok(proxy) = self.proxy(node, COMPONENT).await else {
                continue;
            };
            // Coordinate type 0 is "relative to the screen", which is the
            // one a screenshot is taken in. The reply is one struct, so it
            // deserializes as a tuple inside a tuple.
            let Ok(((x, y, w, h),)) = proxy
                .call::<_, _, ((i32, i32, i32, i32),)>("GetExtents", &(0_u32))
                .await
            else {
                continue;
            };
            if w < 200 || h < 200 {
                continue;
            }
            return Some((
                x - AROUND,
                y - AROUND,
                u16::try_from(w + AROUND * 2).unwrap_or(u16::MAX),
                u16::try_from(h + AROUND * 2).unwrap_or(u16::MAX),
            ));
        }
        None
    }

    /// Wait until the window says something is true, and say what it held
    /// instead if it never does.
    pub async fn until<F>(&self, what: &str, settled: F) -> Result<Vec<Node>>
    where
        F: Fn(&Vec<Node>) -> bool,
    {
        let deadline = std::time::Instant::now() + PATIENCE;
        let mut last = Vec::new();
        while std::time::Instant::now() < deadline {
            last = self.tree().await?;
            if settled(&last) {
                return Ok(last);
            }
            tokio::time::sleep(POLL).await;
        }
        let named: Vec<String> = last
            .iter()
            .filter(|n| !n.name.is_empty())
            .map(|n| format!("{} {:?}", n.role, n.name))
            .collect();
        Err(Error(format!(
            "waited {}s and {what} was still not true.
On screen: {named:#?}",
            PATIENCE.as_secs()
        )))
    }

    /// Shut a channel menu again by pressing the button it hangs off.
    ///
    /// Necessary, not tidiness: a popover left open stays in the widget
    /// tree, so the next scenario's "the button called Leave channel"
    /// finds the previous channel's. That is how this test first claimed
    /// Town Square could be left.
    pub async fn close_menu(&self, channel: &str) -> Result<()> {
        self.press(channel).await?;
        self.until(&format!("{channel}'s menu is shut"), |tree| {
            !tree.has("button", "Leave channel")
        })
        .await
        .map(|_| ())
    }

    /// Press a button by its name, once the window is showing one.
    pub async fn press(&self, name: &str) -> Result<()> {
        // A toggle and a switch press the same way: one action, the first.
        const PRESSABLE: [&str; 4] = ["button", "push button", "toggle button", "switch"];
        let tree = self
            .until(&format!("a button called {name:?} is on screen"), |tree| {
                PRESSABLE.iter().any(|role| tree.has(role, name))
            })
            .await?;
        // The exact name first: "Save" must not find "Saved messages".
        let button = tree
            .iter()
            .find(|n| PRESSABLE.contains(&n.role.as_str()) && n.name == name)
            .or_else(|| PRESSABLE.iter().find_map(|role| tree.find(role, name).ok()))
            .ok_or_else(|| Error(format!("no button called {name:?}")))?;
        self.click(button).await
    }
}

/// Find widgets in a flattened tree.
pub trait Find {
    /// The first widget of this role whose name contains `text`.
    fn find(&self, role: &str, text: &str) -> Result<&Node>;
    /// The first widget of this role that comes after `after` in layout order.
    fn find_after(&self, role: &str, text: &str, after: usize) -> Result<&Node>;
    /// Where a widget is in the list, for `find_after`.
    fn position(&self, role: &str, text: &str) -> Result<usize>;
    /// Whether anything of this role carries this name.
    fn has(&self, role: &str, text: &str) -> bool;
}

impl Find for Vec<Node> {
    fn find(&self, role: &str, text: &str) -> Result<&Node> {
        self.iter()
            .find(|n| n.role == role && n.name.contains(text))
            .ok_or_else(|| missing(self, role, text))
    }

    fn find_after(&self, role: &str, text: &str, after: usize) -> Result<&Node> {
        self.iter()
            .skip(after)
            .find(|n| n.role == role && n.name.contains(text))
            .ok_or_else(|| missing(self, role, text))
    }

    fn position(&self, role: &str, text: &str) -> Result<usize> {
        self.iter()
            .position(|n| n.role == role && n.name.contains(text))
            .ok_or_else(|| missing(self, role, text))
    }

    fn has(&self, role: &str, text: &str) -> bool {
        self.iter().any(|n| n.role == role && n.name.contains(text))
    }
}

/// A failure that says what *was* there, because "not found" on its own
/// sends you back to dumping the tree by hand every time.
/// Whether a D-Bus error says the object is no longer there -- as
/// opposed to the bus being down or the interface being wrong, which
/// are failures worth reporting.
fn gone(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod" | "org.freedesktop.DBus.Error.UnknownObject"
        ),
        _ => false,
    }
}

fn missing(tree: &[Node], role: &str, text: &str) -> Error {
    let same_role: Vec<&str> = tree
        .iter()
        .filter(|n| n.role == role)
        .map(|n| n.name.as_str())
        .collect();
    Error(format!(
        "no {role} named {text:?}. {} widget(s) of that role on screen: {same_role:?}",
        same_role.len()
    ))
}
