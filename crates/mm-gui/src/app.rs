//! The window.
//!
//! Three rules hold here, and they are the whole design:
//!
//! 1. **The GTK main thread owns the state.** There is one
//!    `Rc<RefCell<mm_core::Store>>` and no mutex. A reduce is index
//!    bookkeeping — microseconds — so sharing it with the network thread would
//!    buy nothing and cost determinism.
//! 2. **`block_on` appears nowhere.** Tokio lives on its own thread and
//!    reaches the GUI only through a channel.
//! 3. **`glib::spawn_future_local` is called exactly once**, in [`run`]. Every
//!    per-widget spawn is a place where a future outlives its widget, so
//!    widgets send commands instead of spawning.
//!
//! On the post list: it is a `gtk::ListView` over a custom `gio::ListModel`
//! whose items hold only an id, not a widget per post. A factory
//! materializes one widget set per item, which at a hundred thousand posts
//! means the application never starts. That is measured, not assumed — see
//! `docs/gtk-notes.md`, which also records why "am I at the bottom?" must
//! never be computed from the scroll adjustment.

use crate::bridge::{Command, Event};
use crate::login::LoginPage;
use crate::row::{Attachment, BlockKind, BlockView, PostRow, PostRowFields};
// adw::prelude re-exports gtk::prelude, so this one import covers both.
use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use mm_core::{PageAnchor, Store, Threading};
use mm_types::{ChannelId, PostId, UserId};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const APP_ID: &str = "dev.legion.mattermost_rs";

/// Overrides the application id, which is what GTK keys single-instance on.
///
/// Launching a second time normally hands off to the running window and
/// exits — correct behaviour, and what makes `mattermost://` links open in the
/// window you already have. It also makes it impossible to run a second
/// instance against a different server, which is exactly what you want while
/// developing or when testing a change without disturbing a live session.
/// Pair it with `XDG_STATE_HOME`, which separates the stored session.
///
/// The Electron client has the same affordance under the name `--dataDir`.
const APP_ID_OVERRIDE: &str = "MMRS_APP_ID";

/// Everything the window needs to talk to the outside world.
#[derive(Debug)]
pub struct Wiring {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub events: async_channel::Receiver<Event>,
}

struct Ui {
    store: Rc<RefCell<Store>>,
    /// Swaps between the sign-in screen and the chat view. The window starts
    /// on sign-in and only moves once the engine reports a session, so there
    /// is never a moment where an empty chat view is shown as though it were
    /// loading.
    stack: gtk::Stack,
    login: Rc<LoginPage>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    selected: Rc<RefCell<Option<ChannelId>>>,
    sidebar: gtk::ListBox,
    identity: gtk::Label,
    /// Decoded avatars, by user. A texture is a GPU-side object and must be
    /// built on this thread, so the engine sends bytes and the decode
    /// happens here.
    avatars: Rc<RefCell<HashMap<UserId, gdk::Texture>>>,
    /// Decoded images attached to messages, by file id. Seeded from the
    /// `mini_preview` thumbnail that already rides in the metadata, then
    /// replaced by the real image when it arrives.
    images: Rc<RefCell<HashMap<mm_types::FileId, gdk::Texture>>>,
    /// The thread pane: its model, its view, the panel itself and which
    /// root it is showing.
    thread_posts: gio::ListStore,
    thread_view: gtk::ListView,
    thread_pane: gtk::Box,
    conversation: gtk::Paned,
    thread_root: Rc<RefCell<Option<PostId>>>,
    /// Where the reader was when they opened the channel, for the "new
    /// messages" line. Captured before marking read, because marking read
    /// moves it.
    unread_from: Rc<RefCell<Option<mm_types::Timestamp>>>,
    /// One backfill request at a time. The top edge fires repeatedly while
    /// the user sits there, and without this each firing would start another
    /// identical request.
    loading_older: Rc<std::cell::Cell<bool>>,
    /// After a page of history is prepended, the post the view was resting
    /// on. The list is rebuilt rather than spliced, so without this the
    /// reader would be thrown to the top of sixty newly loaded messages.
    restore_to: Rc<RefCell<Option<PostId>>>,
    /// Who has already been asked for. Without this, every redraw of a
    /// channel would re-request every face in it.
    asked: Rc<RefCell<HashSet<UserId>>>,
    /// The same, for attached images.
    asked_files: Rc<RefCell<HashSet<mm_types::FileId>>>,
    posts: gio::ListStore,
    post_view: gtk::ListView,
    scroller: gtk::ScrolledWindow,
    /// Whether the view should follow new messages.
    ///
    /// Set when a channel is opened, and cleared as soon as the reader
    /// scrolls away from the bottom. Without it, following would yank the
    /// view down while somebody is reading history.
    stick_to_bottom: Rc<std::cell::Cell<bool>>,
    status: gtk::Label,
    title: adw::WindowTitle,
    composer: gtk::Entry,
}

/// Build the window and run the GTK main loop.
#[must_use]
pub fn run(wiring: Wiring) -> glib::ExitCode {
    let app_id = std::env::var(APP_ID_OVERRIDE).unwrap_or_else(|_| APP_ID.to_owned());
    let app = adw::Application::builder().application_id(app_id).build();
    let wiring = Rc::new(RefCell::new(Some(wiring)));

    app.connect_activate(move |app| {
        let Some(Wiring { commands, events }) = wiring.borrow_mut().take() else {
            // A second activation (another launch while this one is running)
            // must present the existing window rather than build a second one.
            if let Some(window) = app.windows().first() {
                window.present();
            }
            return;
        };
        let ui = Rc::new(build(app, &commands));
        ui.commands.send(Command::Sync).ok();

        // THE one bridge. Children never spawn; they send commands.
        let pump = Rc::clone(&ui);
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                pump.on_event(event);
            }
            pump.set_status("the engine stopped");
        });
    });

    app.run_with_args::<&str>(&[])
}

/// The thread panel: a post list, a heading with a close button, and a
/// composer that replies into the open thread.
///
/// Hidden until a thread is opened — an empty panel permanently taking a
/// third of the window would be worse than no panel.
fn build_thread_pane(
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) -> (
    gio::ListStore,
    gtk::ListView,
    gtk::Box,
    gtk::Button,
    gtk::Entry,
) {
    let (thread_posts, thread_view, thread_scroller) = build_post_list(commands);
    let thread_title = gtk::Label::builder()
        .label("Thread")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let thread_close = gtk::Button::from_icon_name("window-close-symbolic");
    thread_close.add_css_class("flat");
    thread_close.set_tooltip_text(Some("Close thread"));
    let thread_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    thread_header.set_margin_start(12);
    thread_header.set_margin_end(6);
    thread_header.set_margin_top(8);
    thread_header.set_margin_bottom(4);
    thread_header.append(&thread_title);
    thread_header.append(&thread_close);
    let thread_composer = gtk::Entry::builder()
        .placeholder_text("Reply…")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(12)
        .build();
    let thread_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    thread_pane.append(&thread_header);
    thread_pane.append(&thread_scroller);
    thread_pane.append(&thread_composer);
    thread_pane.set_visible(false);

    // Wide enough to read a reply in. Without this the Paned hands the
    // hidden child its minimum, and opening a thread produced a column
    // about one word wide.
    thread_pane.set_size_request(THREAD_PANE_WIDTH, -1);
    (
        thread_posts,
        thread_view,
        thread_pane,
        thread_close,
        thread_composer,
    )
}

/// The channel list, under a line naming the account this window shows.
fn build_sidebar() -> (gtk::ListBox, gtk::Label, gtk::Box) {
    let sidebar = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .child(&sidebar)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    // Which account this window is looking at. It used to live in the window
    // subtitle, which the channel header now owns; this is where Mattermost
    // puts the server and team too.
    let identity = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .margin_start(12)
        .margin_end(12)
        .margin_top(10)
        .margin_bottom(6)
        .css_classes(["dim-label", "caption"])
        .label("connecting…")
        .build();
    let sidebar_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_pane.set_size_request(260, -1);
    sidebar_pane.append(&identity);
    sidebar_pane.append(&sidebar_scroll);
    (sidebar, identity, sidebar_pane)
}

/// Styling this window needs that the theme does not provide.
///
/// Only links: GTK paints them in the desktop's accent colour, which on
/// Ubuntu is orange, and a chat client showing orange links beside a
/// Mattermost client showing blue ones reads as a different application.
/// The colour is Mattermost's own, and it is legible on both themes.
fn install_styles() {
    let css = gtk::CssProvider::new();
    css.load_from_string("link { color: #5d89ea; } link:hover { color: #86a9f2; }");
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build(app: &adw::Application, commands: &tokio::sync::mpsc::UnboundedSender<Command>) -> Ui {
    install_styles();
    let (sidebar, identity, sidebar_pane) = build_sidebar();

    let (posts, post_view, scroller) = build_post_list(commands);

    let composer = gtk::Entry::builder()
        .placeholder_text("Write a message…")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(12)
        .sensitive(false)
        .build();

    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_bottom(4)
        .css_classes(["dim-label", "caption"])
        .label("starting…")
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&scroller);
    content.append(&status);
    content.append(&composer);

    let (thread_posts, thread_view, thread_pane, thread_close, thread_composer) =
        build_thread_pane(commands);

    let conversation = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&content)
        .end_child(&thread_pane)
        .resize_end_child(false)
        .shrink_end_child(false)
        .build();

    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar_pane)
        .end_child(&conversation)
        .resize_start_child(false)
        .position(260)
        .build();

    let title = adw::WindowTitle::new("mattermost-rs", "connecting…");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Reload channels"));
    header.pack_end(&refresh);

    let login = Rc::new(LoginPage::new());
    // No transition. A crossfade queued while the stack is still unmapped
    // leaves `visible_child_name()` reporting the new page while the old one
    // is what actually renders -- which presented as "signed in, but the
    // login form is still on screen".
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::None)
        .build();
    stack.add_named(&login.root, Some("login"));
    stack.add_named(&split, Some("main"));
    stack.set_visible_child_name("login");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&stack);

    // When the application id is overridden for development, put it in the
    // window title too. Two instances otherwise present as two windows with
    // the same name and no way to tell them apart -- which is how a test
    // capture ended up photographing the wrong one.
    let window_title = std::env::var(APP_ID_OVERRIDE).map_or_else(
        |_| "mattermost-rs".to_owned(),
        |id| format!("mattermost-rs [{id}]"),
    );

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(window_title)
        .default_width(1_040)
        .default_height(720)
        .content(&root)
        .build();
    window.present();

    let ui = Ui {
        store: Rc::new(RefCell::new(Store::new(UserId::default(), Threading::Flat))),
        commands: commands.clone(),
        selected: Rc::new(RefCell::new(None)),
        sidebar: sidebar.clone(),
        identity: identity.clone(),
        thread_posts: thread_posts.clone(),
        thread_view: thread_view.clone(),
        thread_pane: thread_pane.clone(),
        conversation: conversation.clone(),
        thread_root: Rc::new(RefCell::new(None)),
        unread_from: Rc::new(RefCell::new(None)),
        loading_older: Rc::new(std::cell::Cell::new(false)),
        restore_to: Rc::new(RefCell::new(None)),
        avatars: Rc::new(RefCell::new(HashMap::new())),
        images: Rc::new(RefCell::new(HashMap::new())),
        asked: Rc::new(RefCell::new(HashSet::new())),
        asked_files: Rc::new(RefCell::new(HashSet::new())),
        posts,
        post_view,
        scroller,
        stick_to_bottom: Rc::new(std::cell::Cell::new(true)),
        status,
        title,
        composer: composer.clone(),
        stack,
        login: Rc::clone(&login),
    };

    wire(&ui, &refresh, &sidebar, &composer, commands);
    wire_thread(&ui, &thread_close, &thread_composer, commands);
    wire_login(&login, commands);
    ui
}

/// Connect the sign-in form.
fn wire_login(login: &Rc<LoginPage>, commands: &tokio::sync::mpsc::UnboundedSender<Command>) {
    let submit = {
        let login = Rc::clone(login);
        let commands = commands.clone();
        move || {
            let values = login.values();
            if let Some(problem) = values.problem() {
                login.show_error(problem);
                return;
            }
            login.set_busy(true);
            login.show_hint("Signing in…");
            commands
                .send(Command::LogIn {
                    server: values.server,
                    login_id: values.login_id,
                    password: values.password,
                    mfa: values.mfa,
                })
                .ok();
            // The request has been built; the field need not keep holding it.
            login.clear_password();
        }
    };

    {
        let submit = submit.clone();
        login.submit.connect_clicked(move |_| submit());
    }
    // Enter in any field submits, which is what every other login form does.
    for entry in [&login.server, &login.login_id, &login.mfa] {
        let submit = submit.clone();
        entry.connect_entry_activated(move |_| submit());
    }
    {
        let submit = submit.clone();
        login.password.connect_entry_activated(move |_| submit());
    }
}

/// Connect the thread pane: closing it, and replying into it.
fn wire_thread(
    ui: &Ui,
    close: &gtk::Button,
    composer: &gtk::Entry,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    {
        let root = Rc::clone(&ui.thread_root);
        let pane = ui.thread_pane.clone();
        let posts = ui.thread_posts.clone();
        close.connect_clicked(move |_| {
            *root.borrow_mut() = None;
            posts.remove_all();
            pane.set_visible(false);
        });
    }

    let commands = commands.clone();
    let root = Rc::clone(&ui.thread_root);
    let selected = Rc::clone(&ui.selected);
    composer.connect_activate(move |entry| {
        let text = entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        // Both are needed: a reply carries the thread it belongs to *and*
        // the channel it is posted in.
        let (Some(channel), Some(root)) = (selected.borrow().clone(), root.borrow().clone()) else {
            return;
        };
        commands
            .send(Command::Send {
                channel,
                text,
                root: Some(root),
            })
            .ok();
        entry.set_text("");
    });
}

/// Connect the widgets to the command channel.
///
/// Separate from `build` only so neither function grows past the point where
/// it can be read in one go.
/// Connect choosing a channel: what it opens, and what it closes.
fn wire_selection(
    ui: &Ui,
    sidebar: &gtk::ListBox,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    let commands = commands.clone();
    let selected = Rc::clone(&ui.selected);
    let stick = Rc::clone(&ui.stick_to_bottom);
    let thread_root = Rc::clone(&ui.thread_root);
    let thread_posts = ui.thread_posts.clone();
    let thread_pane = ui.thread_pane.clone();
    let unread_from = Rc::clone(&ui.unread_from);
    let store = Rc::clone(&ui.store);
    sidebar.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let id = ChannelId::new(row.widget_name().to_string());

        // `refresh_sidebar` rebuilds every row and re-selects the open
        // channel, so this handler fires on its own output. Without this
        // guard each WebSocket event -- including a stranger's `typing` --
        // re-fetched the whole channel over the network, rebuilt the post
        // list and wrote a mark-read. Only a real change of channel is a
        // user action.
        if selected.borrow().as_ref() == Some(&id) {
            return;
        }

        // Opening a channel always starts at the newest message, and
        // closes whatever thread was open: a thread belongs to the
        // channel it came from, and leaving it up beside a different one
        // shows a conversation that is not there.
        stick.set(true);
        // Before `MarkRead` below, which moves it.
        *unread_from.borrow_mut() = store.borrow().last_viewed(&id);
        *thread_root.borrow_mut() = None;
        thread_posts.remove_all();
        thread_pane.set_visible(false);
        *selected.borrow_mut() = Some(id.clone());
        commands.send(Command::OpenChannel(id.clone())).ok();
        commands.send(Command::MarkRead(id)).ok();
    });
}

fn wire(
    ui: &Ui,
    refresh: &gtk::Button,
    sidebar: &gtk::ListBox,
    composer: &gtk::Entry,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    {
        let commands = commands.clone();
        refresh.connect_clicked(move |_| {
            commands.send(Command::Sync).ok();
        });
    }
    {
        // Following new messages, driven by what the reader did rather than by
        // arithmetic on the scroll position.
        //
        // The arithmetic version was tried and does not work: `upper` is an
        // estimate GTK revises as rows are realized, so a view pinned to the
        // bottom stops looking pinned the moment the estimate grows, and the
        // flag clears itself. Scroll gestures and edge hits are facts about
        // intent and do not drift.
        let scrolling_up = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC,
        );
        {
            let stick = Rc::clone(&ui.stick_to_bottom);
            scrolling_up.connect_scroll(move |_, _, dy| {
                // Any upward scroll means the reader is looking at history and
                // must not be yanked away from it.
                if dy < 0.0 {
                    stick.set(false);
                }
                glib::Propagation::Proceed
            });
        }
        ui.scroller.add_controller(scrolling_up);

        // Arriving back at the bottom opts back in; arriving at the top
        // asks for the page of history before what is held.
        let stick = Rc::clone(&ui.stick_to_bottom);
        let loading = Rc::clone(&ui.loading_older);
        let restore = Rc::clone(&ui.restore_to);
        let selected = Rc::clone(&ui.selected);
        let store = Rc::clone(&ui.store);
        let posts = ui.posts.clone();
        let commands = commands.clone();
        ui.scroller.connect_edge_reached(move |_, edge| match edge {
            gtk::PositionType::Bottom => stick.set(true),
            gtk::PositionType::Top => {
                let Some(channel) = selected.borrow().clone() else {
                    return;
                };
                // The edge keeps firing while the reader sits at the top, so
                // without the in-flight flag each firing starts another
                // identical request.
                if loading.get() || store.borrow().older_complete(&channel) {
                    return;
                }
                // The oldest post held is both what to ask the server for
                // and what to scroll back to afterwards. Row 0 is a date
                // separator and carries no id, so this walks to the first
                // real message -- reading row 0 would find an empty id and
                // never backfill at all.
                let Some(oldest) = (0..posts.n_items())
                    .filter_map(|i| posts.item(i).and_downcast::<PostRow>())
                    .map(|row| PostId::new(row.id()))
                    .find(|id| !id.as_str().is_empty())
                else {
                    return;
                };
                loading.set(true);
                *restore.borrow_mut() = Some(oldest.clone());
                commands
                    .send(Command::LoadOlder {
                        channel,
                        before: oldest,
                    })
                    .ok();
            }
            _ => {}
        });
    }
    wire_selection(ui, sidebar, commands);

    {
        let commands = commands.clone();
        let selected = Rc::clone(&ui.selected);
        composer.connect_activate(move |entry| {
            let text = entry.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            let Some(channel) = selected.borrow().clone() else {
                return;
            };
            commands
                .send(Command::Send {
                    channel,
                    text,
                    root: None,
                })
                .ok();
            entry.set_text("");
        });
    }
}

/// Render a parsed message to Pango markup.
///
/// Block structure is flattened to lines: block-level widgets per paragraph
/// arrive with the component tree, and emitting them as separate labels now
/// would mean rebuilding the row factory for a layout that is about to change.
/// Turn a parsed message into the blocks a row draws.
///
/// A code block is a box and a rule is a line; neither is expressible as
/// Pango markup, which is why this returns a list rather than one string.
/// The common case -- a single paragraph of plain text -- comes back as one
/// block that needs no markup parsing at all.
fn render_blocks(doc: &mm_md::Document) -> Vec<BlockView> {
    if let Some(plain) = doc.plain_text() {
        return vec![BlockView {
            kind: BlockKind::Text,
            text: plain.to_owned(),
            markup: false,
            quoted: false,
            cells: Vec::new(),
        }];
    }
    let mut out = Vec::new();
    push_blocks(&doc.blocks, false, &mut out);
    out
}

fn push_blocks(blocks: &[mm_md::Block], quoted: bool, out: &mut Vec<BlockView>) {
    for block in blocks {
        match block {
            mm_md::Block::Paragraph(inlines) | mm_md::Block::Heading { inlines, .. } => {
                out.push(BlockView {
                    kind: BlockKind::Text,
                    text: mm_md::to_pango(inlines),
                    markup: true,
                    quoted,
                    cells: Vec::new(),
                });
            }
            mm_md::Block::ListItem(inlines) => out.push(BlockView {
                kind: BlockKind::Bullet,
                text: mm_md::to_pango(inlines),
                markup: true,
                quoted,
                cells: Vec::new(),
            }),
            mm_md::Block::Table(rows) => out.push(BlockView {
                kind: BlockKind::Table,
                text: String::new(),
                markup: true,
                quoted,
                cells: rows
                    .iter()
                    .map(|row| row.iter().map(|cell| mm_md::to_pango(cell)).collect())
                    .collect(),
            }),
            mm_md::Block::Code { code, .. } => out.push(BlockView {
                kind: BlockKind::Code,
                // Code is literal by definition: it is the one thing that
                // must never be parsed as markup.
                text: code.trim_end().to_owned(),
                markup: false,
                quoted,
                cells: Vec::new(),
            }),
            // A quote inside a quote still draws one bar. Mattermost's own
            // rendering nests them, but a second bar on a chat message is
            // noise and the depth is not information the reader needs.
            mm_md::Block::Quote(inner) => push_blocks(inner, true, out),
            mm_md::Block::Rule => out.push(BlockView {
                kind: BlockKind::Rule,
                text: String::new(),
                markup: false,
                quoted,
                cells: Vec::new(),
            }),
        }
    }
}

/// The trailing line: what is attached, and how many replies there are.
///
/// Without this a post whose only content is a file renders as an author, a
/// timestamp and nothing at all, which reads as the client being broken.
/// The line under a message for anything that is not the message: files,
/// and nothing else.
///
/// Replies used to be listed here too, which meant a post with a thread
/// said "3 replies" twice -- once as text and once as the button that
/// actually opens it.
fn describe_extras(post: &mm_types::Post) -> String {
    let mut parts: Vec<String> = Vec::new();
    for file in post.attachments() {
        // An image drawn in the row names itself. The line is still where a
        // document goes, and where an image goes whose dimensions the server
        // did not state -- that one is not drawn, so without this it would
        // vanish from the message entirely.
        if file.is_image() && file.fitted(IMAGE_MAX_WIDTH, IMAGE_MAX_HEIGHT).is_some() {
            continue;
        }
        let icon = if file.is_image() { "🖼" } else { "📎" };
        let name = if file.name.is_empty() {
            // Some endpoints omit metadata, leaving only the id. Saying a file
            // is there beats showing nothing.
            "attachment".to_owned()
        } else {
            file.name.clone()
        };
        let size = file.human_size();
        if size.is_empty() {
            parts.push(format!("{icon} {name}"));
        } else {
            parts.push(format!("{icon} {name} ({size})"));
        }
    }
    parts.join("   ")
}

/// `HH:MM` in local time.
/// The reader's offset from UTC, east positive.
///
/// Read here and passed into `mm-core` rather than read there: a calendar day
/// depends on the timezone, and the projection stays a total function of its
/// arguments.
fn local_utc_offset_secs() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

/// The label for a day separator.
///
/// "Today" and "Yesterday" are relative to the moment of drawing, which is
/// why this is here and not in `mm-core`: it is the one piece of this that
/// genuinely needs a clock.
fn describe_day(at: mm_types::Timestamp, date: &mm_core::CivilDate) -> String {
    use chrono::{Datelike, Local, TimeZone};

    let today = Local::now().date_naive();
    let is = |day: chrono::NaiveDate| {
        i64::from(day.year()) == date.year && day.month() == date.month && day.day() == date.day
    };
    if is(today) {
        return "Today".to_owned();
    }
    if today.pred_opt().is_some_and(is) {
        return "Yesterday".to_owned();
    }
    match Local.timestamp_millis_opt(at.millis()) {
        chrono::offset::LocalResult::Single(dt) if dt.year() == today.year() => {
            dt.format("%A, %B %-d").to_string()
        }
        chrono::offset::LocalResult::Single(dt) => dt.format("%A, %B %-d, %Y").to_string(),
        // An unrepresentable date is not worth failing a render over; the
        // rule still draws, just without a label.
        _ => String::new(),
    }
}

fn clock_time(millis: i64) -> String {
    use chrono::{Local, TimeZone};
    // An out-of-range or ambiguous timestamp is not worth failing a render
    // over; an empty time column is better than no message.
    if let chrono::offset::LocalResult::Single(dt) = Local.timestamp_millis_opt(millis) {
        dt.format("%H:%M").to_string()
    } else {
        String::new()
    }
}

/// The message list: a `ListView` over a model of ids, with one recycled
/// widget set per visible row.
///
/// Not a per-post widget: a factory that materializes one widget set per item
/// means a hundred-thousand-post channel never opens. That is measured, not
/// assumed -- see `docs/gtk-notes.md`.
/// Build the widget tree one list row is recycled through.
fn setup_post_row(item: &gtk::ListItem) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(4);
    row.set_margin_bottom(4);

    // The day boundary, drawn as a rule with the date sitting in it.
    // Every row carries one and all but the first of each day hide it,
    // because ListView recycles a fixed set of widgets and a row that
    // could not become a separator would need a second factory.
    let day = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    day.set_margin_top(8);
    day.set_margin_bottom(8);
    day.append(
        &gtk::Separator::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build(),
    );
    day.append(
        &gtk::Label::builder()
            .css_classes(["dim-label", "caption-heading"])
            .build(),
    );
    day.append(
        &gtk::Separator::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build(),
    );

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let author = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let time = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    let bot = gtk::Label::builder()
        .label("BOT")
        .css_classes(["caption-heading", "dim-label"])
        .build();
    header.append(&author);
    header.append(&bot);
    header.append(&time);
    // A box of blocks rather than one label: a code block is a panel and a
    // rule is a line, and neither is expressible as Pango markup.
    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    // Attachments and "N replies" -- the line that keeps a post whose
    // only content is a file from rendering as an author and nothing.
    let meta = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    // Reaction chips. Rebuilt on every bind rather than recycled: a row can
    // carry none one moment and four the next, and four buttons is cheap.
    let reactions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    reactions.set_margin_top(2);

    // The message itself sits beside a fixed gutter holding the author's
    // face, so grouped messages line up under the one that named them.
    // `adw::Avatar` draws initials when there is no picture, which means a
    // row never has to wait on the network to look finished.
    let face = adw::Avatar::builder()
        .size(32)
        .show_initials(true)
        .valign(gtk::Align::Start)
        .build();
    let main = gtk::Box::new(gtk::Orientation::Vertical, 2);
    main.set_hexpand(true);
    // Attached images, above the file line that names them. Rebuilt on each
    // bind like the reaction chips: a recycled row can go from none to three.
    let images = gtk::Box::new(gtk::Orientation::Vertical, 4);
    images.set_margin_top(4);
    images.set_halign(gtk::Align::Start);

    // Webhook attachments. A channel of alerts is nothing but these, so
    // they sit directly under the body where a message would be.
    let cards = gtk::Box::new(gtk::Orientation::Vertical, 6);
    cards.set_margin_top(2);

    main.append(&header);
    main.append(&body);
    main.append(&cards);
    main.append(&images);
    main.append(&meta);
    main.append(&reactions);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(&face);
    line.append(&main);

    row.append(&day);
    row.append(&line);
    item.set_child(Some(&row));
}

/// Fill a recycled row from the model item bound to it.
/// Draw a row that is a day boundary or the "new messages" line.
fn bind_separator_row(day: &gtk::Box, line: &gtk::Box, data: &PostRow) {
    // The "new messages" line is the same rule in the accent colour, so
    // it reads as a marker rather than as another date.
    let accent = data.is_unread_mark();
    for part in [day.first_child(), day.last_child()].into_iter().flatten() {
        if accent {
            part.add_css_class("accent");
        } else {
            part.remove_css_class("accent");
        }
    }
    if let Some(label) = day
        .first_child()
        .and_then(|c| c.next_sibling())
        .and_downcast::<gtk::Label>()
    {
        label.set_text(&data.body());
        if accent {
            label.add_css_class("accent");
            label.remove_css_class("dim-label");
        } else {
            label.remove_css_class("accent");
            label.add_css_class("dim-label");
        }
    }
    line.set_visible(false);
}

/// Fill a row's image strip. Rebuilt rather than recycled: a reused row can
/// go from no images to three.
/// Put the author's name on a row, and mark it when a bot wrote it.
fn bind_author(author: &gtk::Label, data: &PostRow) {
    // A system notice is named "System", the way Mattermost names it --
    // not blank, which read as a message from nobody.
    author.set_text(&data.author());
    if let Some(tag) = author.next_sibling() {
        tag.set_visible(data.is_bot());
    }
}

/// Fill a row's webhook attachments.
///
/// Each one is a coloured bar beside a column of text, which is how every
/// other Mattermost client draws them and how an alert channel is read at a
/// glance: the colour is the severity.
fn bind_cards(cards: &gtk::Box, data: &PostRow) {
    while let Some(old) = cards.first_child() {
        cards.remove(&old);
    }
    let views = data.cards();
    cards.set_visible(!views.is_empty());

    for view in views {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.set_margin_start(10);
        column.set_hexpand(true);

        if !view.pretext.is_empty() {
            column.append(&card_label(&view.pretext, false, &["dim-label"]));
        }
        if !view.title.is_empty() {
            // A linked title is a link; an unlinked one is just bold. Pango
            // markup is how a Label carries a link at all, and the title is
            // literal text, so it has to be escaped on the way in.
            if view.title_link.is_empty() {
                column.append(&card_label(&view.title, false, &["heading"]));
            } else {
                let markup = format!(
                    "<a href=\"{}\">{}</a>",
                    mm_md::escape(&view.title_link),
                    mm_md::escape(&view.title)
                );
                column.append(&card_label(&markup, true, &["heading"]));
            }
        }
        if !view.blocks.is_empty() {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            fill_blocks(&body, &view.blocks);
            column.append(&body);
        }
        for (title, value) in &view.fields {
            if !title.is_empty() {
                column.append(&card_label(title, false, &["caption-heading"]));
            }
            if !value.is_empty() {
                column.append(&card_label(value, false, &[]));
            }
        }
        if !view.footer.is_empty() {
            column.append(&card_label(&view.footer, false, &["dim-label", "caption"]));
        }

        let card = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        if let Some((r, g, b)) = view.rgb {
            card.append(&colour_bar(r, g, b));
        }
        card.append(&column);
        cards.append(&card);
    }
}

/// Build the widgets for a message body, a block at a time.
///
/// A code block becomes a padded box rather than a run of monospace text,
/// which is what every other Mattermost client draws and what makes an
/// alert's details readable at all.
fn fill_blocks(into: &gtk::Box, blocks: &[BlockView]) {
    while let Some(old) = into.first_child() {
        into.remove(&old);
    }
    into.set_visible(!blocks.is_empty());

    for block in blocks {
        let widget: gtk::Widget = match block.kind {
            BlockKind::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
            BlockKind::Code => {
                let code = card_label(&block.text, false, &["monospace"]);
                code.set_margin_start(10);
                code.set_margin_end(10);
                code.set_margin_top(6);
                code.set_margin_bottom(6);
                let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
                panel.append(&code);
                // "card" is libadwaita's panel styling: a background a shade
                // off the window's, and rounded corners.
                panel.add_css_class("card");
                panel.upcast()
            }
            BlockKind::Bullet => {
                let item = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                item.set_margin_start(6);
                let bullet = card_label("•", false, &["dim-label"]);
                // Labels expand by default so they can wrap; a bullet that
                // expands takes the whole line and pushes its own text to
                // the far right.
                bullet.set_hexpand(false);
                item.append(&bullet);
                item.append(&card_label(&block.text, block.markup, &[]));
                item.upcast()
            }
            BlockKind::Table => build_table(&block.cells).upcast(),
            BlockKind::Text => card_label(&block.text, block.markup, &[]).upcast(),
        };

        if block.quoted {
            // A quote is a bar down the left, like an attachment, and dimmed
            // -- it is somebody else's words.
            let quoted = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            quoted.append(&quote_bar());
            widget.add_css_class("dim-label");
            widget.set_hexpand(true);
            quoted.append(&widget);
            into.append(&quoted);
        } else {
            into.append(&widget);
        }
    }
}

/// A GFM table, as a grid with its header picked out.
fn build_table(rows: &[Vec<String>]) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .row_spacing(2)
        .column_spacing(16)
        .margin_start(10)
        .margin_end(10)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    grid.add_css_class("card");
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            // The header is the first row, by GFM's definition.
            let classes: &[&str] = if r == 0 { &["heading"] } else { &[] };
            let label = card_label(cell, true, classes);
            // A cell sizes to its content; letting one expand would push the
            // rest of the table off the edge.
            label.set_hexpand(false);
            label.set_wrap(false);
            let (Ok(c), Ok(r)) = (i32::try_from(c), i32::try_from(r)) else {
                continue;
            };
            grid.attach(&label, c, r, 1, 1);
        }
    }
    grid
}

/// The bar beside a block quote, in the theme's own dim colour rather than
/// a colour of its own.
fn quote_bar() -> gtk::Separator {
    let bar = gtk::Separator::new(gtk::Orientation::Vertical);
    bar.add_css_class("dim-label");
    bar
}

/// One line of text inside a message or an attachment.
fn card_label(text: &str, markup: bool, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        // A wrapping label still reports its *unwrapped* width as its
        // natural size, so the row asks for that width and a long message
        // runs off the right-hand edge. `NaturalWrapMode::None` makes the
        // natural size the wrapped one, which is what lets it fit.
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        // And break inside a word when a word is wider than the window: a
        // bare URL or a long code span is one unbreakable token, and one
        // such token in one message widened every row in the channel and
        // pushed the rest of the text off the edge.
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .hexpand(true)
        .selectable(true)
        .css_classes(classes.to_vec())
        .build();
    // set_markup on text that is not markup makes GTK log and drop it.
    if markup {
        label.set_markup(text);
    } else {
        label.set_text(text);
    }
    label
}

/// The severity bar down an attachment's left-hand side.
///
/// Drawn rather than styled: the colour comes from the payload, and a CSS
/// class per arbitrary colour would mean generating stylesheets at runtime.
fn colour_bar(r: u8, g: u8, b: u8) -> gtk::DrawingArea {
    let bar = gtk::DrawingArea::new();
    bar.set_width_request(3);
    bar.set_draw_func(move |_, context, width, height| {
        context.set_source_rgb(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        );
        context.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
        // A failed fill is a bar that does not draw, which is cosmetic; it
        // is not worth taking the message down for.
        let _ = context.fill();
    });
    bar
}

fn bind_images(images: &gtk::Box, data: &PostRow) {
    while let Some(old) = images.first_child() {
        images.remove(&old);
    }
    let attached = data.images();
    images.set_visible(!attached.is_empty());
    for image in attached {
        let picture = gtk::Picture::builder()
            // The row already reserved this much, from the dimensions in the
            // metadata, so the list does not jump when the real image lands
            // on top of the thumbnail.
            .width_request(image.width)
            .height_request(image.height)
            .content_fit(gtk::ContentFit::Contain)
            .build();
        if let Some(texture) = &image.texture {
            picture.set_paintable(Some(texture));
        }
        picture.add_css_class("card");
        images.append(&picture);
    }
}

fn bind_post_row(item: &gtk::ListItem, commands: &tokio::sync::mpsc::UnboundedSender<Command>) {
    let Some(data) = item.item().and_downcast::<PostRow>() else {
        return;
    };
    let Some(row) = item.child().and_downcast::<gtk::Box>() else {
        return;
    };
    // Walk the children by position rather than first/last: there are
    // four now, and last_child() silently became the wrong one when the
    // meta line was added.
    let Some(day) = row.first_child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(line) = day.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(face) = line.first_child().and_downcast::<adw::Avatar>() else {
        return;
    };
    let Some(main) = face.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(header) = main.first_child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(body) = header.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(cards) = body.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(images) = cards.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(meta) = images.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(reactions) = meta.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };

    // A separator is a day label and nothing else.
    let is_separator = data.is_separator();
    day.set_visible(is_separator);
    if is_separator {
        bind_separator_row(&day, &line, &data);
        return;
    }

    line.set_visible(true);
    // A grouped message sits tight under the one above it, which is what
    // makes a block read as one utterance rather than three.
    let grouped = data.is_grouped();
    row.set_margin_top(if grouped { 0 } else { 6 });

    // The face belongs to the message that names its author. A grouped
    // message keeps the *alignment* but not the gutter widget: an avatar
    // hidden with opacity still claims its 32 pixels of height, which pushed
    // every line of a block a row apart and undid the grouping entirely.
    face.set_visible(!grouped);
    main.set_margin_start(if grouped { AVATAR_GUTTER } else { 0 });
    face.set_text(Some(&data.author()));
    face.set_custom_image(data.avatar().as_ref());

    let system = data.is_system();
    // Still the same person talking: the author line would just repeat
    // itself, so the message tucks under the one above.
    header.set_visible(!grouped);
    if let Some(author) = header.first_child().and_downcast::<gtk::Label>() {
        // A join/leave notice is not somebody speaking, so it gets no
        // author line.
        bind_author(&author, &data);
    }
    if let Some(time) = header.last_child().and_downcast::<gtk::Label>() {
        time.set_text(&data.time());
    }

    fill_blocks(&body, &data.blocks());
    if system {
        body.add_css_class("dim-label");
    } else {
        body.remove_css_class("dim-label");
    }

    bind_cards(&cards, &data);
    bind_images(&images, &data);

    let extra = data.meta();
    meta.set_text(&extra);
    meta.set_visible(!extra.is_empty());

    while let Some(chip) = reactions.first_child() {
        reactions.remove(&chip);
    }
    let chips = data.reactions();
    let replies = data.replies();
    reactions.set_visible(replies > 0 || !chips.is_empty());
    // With collapsed threads on -- the server default since 12.1 -- replies
    // are not in the channel at all, so this button is the only way to read
    // them. It comes before the reactions because it is the one that leads
    // somewhere.
    if replies > 0 {
        let open = gtk::Button::builder()
            .label(if replies == 1 {
                "1 reply".to_owned()
            } else {
                format!("{replies} replies")
            })
            .css_classes(["flat", "caption"])
            .build();
        let commands = commands.clone();
        let post = PostId::new(data.id());
        open.connect_clicked(move |_| {
            commands.send(Command::OpenThread(post.clone())).ok();
        });
        reactions.append(&open);
    }
    for chip in chips {
        reactions.append(&reaction_chip(&data.id(), &chip, commands));
    }
}

/// The avatar's width plus the gap after it. A grouped message has no
/// avatar and borrows this as a left margin, so its text stays in line with
/// the message that named its author.
const AVATAR_GUTTER: i32 = 32 + 8;

/// The box an inline image is fitted into. Wide enough to read a
/// screenshot, short enough that one photo does not fill the window.
const IMAGE_MAX_WIDTH: i64 = 420;
const IMAGE_MAX_HEIGHT: i64 = 320;

/// Turn one Slack-style attachment into what the row draws.
///
/// The markdown is rendered here, once per refresh, rather than in the bind
/// callback -- bind runs on every scroll, and a channel of alerts would
/// re-parse every one of them each time it moved.
fn render_card(
    attachment: &mm_types::MessageAttachment,
    store: &Store,
) -> crate::row::AttachmentView {
    let mut doc = mm_md::parse(&attachment.text);
    mm_md::mark_mentions(&mut doc, &|name| store.mentioned(name));
    crate::row::AttachmentView {
        blocks: render_blocks(&doc),
        rgb: attachment.rgb(),
        pretext: attachment.pretext.clone(),
        title: attachment.title.clone(),
        title_link: attachment.title_link.clone(),
        fields: attachment
            .fields
            .iter()
            .map(|f| (f.title.clone(), f.text()))
            .collect(),
        footer: attachment.footer.clone(),
    }
}

/// Decode the base64 JPEG thumbnail the server puts in a file's metadata.
///
/// A couple of kilobytes and no request, which is the whole point: it is
/// what lets a row show something at the right size on the first frame.
fn decode_mini_preview(encoded: &str) -> Option<gdk::Texture> {
    if encoded.is_empty() {
        return None;
    }
    let bytes = glib::base64_decode(encoded);
    gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
}

/// How much of the window a thread takes when it is open.
const THREAD_PANE_WIDTH: i32 = 380;

/// A category heading: not selectable, so arrow keys and the first-channel
/// fallback walk straight past it.
fn section_header(section: &mm_core::SidebarSection) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.set_margin_top(10);
    line.set_margin_bottom(2);
    let name = gtk::Label::builder()
        .label(section.display_name.to_uppercase())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption-heading"])
        .build();
    line.append(&name);

    // A folded category shows what is inside it the only way it can.
    let mentions = section.mentions();
    if section.collapsed && mentions > 0 {
        line.append(
            &gtk::Label::builder()
                .label(mentions.to_string())
                .css_classes(["accent", "caption-heading"])
                .build(),
        );
    }

    let row = gtk::ListBoxRow::builder()
        .child(&line)
        .selectable(false)
        .activatable(false)
        .build();
    row.add_css_class("mmrs-section");
    row
}

/// One reaction, as a button that toggles the signed-in user's own.
fn reaction_chip(
    post: &str,
    summary: &mm_core::ReactionSummary,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) -> gtk::Button {
    // A custom or unknown emoji keeps its shortcode: `:shipit:` says what
    // was meant, where a blank chip says nothing.
    let face = mm_md::glyph(&summary.emoji)
        .map_or_else(|| format!(":{}:", summary.emoji), ToOwned::to_owned);
    let chip = gtk::Button::builder()
        .label(format!("{face} {}", summary.count))
        .css_classes(["flat", "caption"])
        .tooltip_text(format!(":{}:", summary.emoji))
        .build();
    if summary.mine {
        chip.add_css_class("suggested-action");
    }

    let commands = commands.clone();
    let post = PostId::new(post);
    let emoji = summary.emoji.clone();
    // Clicking a reaction you are already part of takes it back, which is
    // the whole interaction: there is no separate "remove" affordance.
    let add = !summary.mine;
    chip.connect_clicked(move |_| {
        commands
            .send(Command::React {
                post: post.clone(),
                emoji: emoji.clone(),
                add,
            })
            .ok();
    });
    chip
}

fn build_post_list(
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) -> (gio::ListStore, gtk::ListView, gtk::ScrolledWindow) {
    let posts = gio::ListStore::new::<PostRow>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            setup_post_row(item);
        }
    });
    let sender = commands.clone();
    factory.connect_bind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            bind_post_row(item, &sender);
        }
    });
    let post_view = gtk::ListView::builder()
        .model(&gtk::NoSelection::new(Some(posts.clone())))
        .factory(&factory)
        .single_click_activate(false)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&post_view)
        .vexpand(true)
        // Never horizontally. Without this the list takes the width of its
        // widest row -- its *natural* width -- and a long message runs off
        // the right-hand edge behind a horizontal scrollbar instead of
        // wrapping. Forbidding the scrollbar is what forces the rows to the
        // viewport's width, which is what makes a wrapping label wrap.
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    (posts, post_view, scroller)
}

impl Ui {
    /// Apply one engine event.
    ///
    /// `borrow_mut` on the store happens here and nowhere else, which is the
    /// rule that keeps the GUI a projection rather than a second source of
    /// truth.
    fn on_event(&self, event: Event) {
        match event {
            Event::NeedLogin => {
                self.stack.set_visible_child_name("login");
                self.login.set_busy(false);
                self.login
                    .show_hint("Your password is sent to your server and never stored.");
                self.identity.set_text("not signed in");
                // The title bar still said "connecting…" over a sign-in
                // form, which is a window telling the user something that is
                // not true.
                self.title.set_subtitle("");
            }
            Event::LoginFailed { message, needs_mfa } => {
                self.login.set_busy(false);
                if needs_mfa {
                    self.login.ask_for_mfa();
                    self.login
                        .show_hint("Enter the code from your authenticator app.");
                } else {
                    self.login.show_error(&message);
                }
            }
            Event::Connected {
                server,
                me,
                username,
            } => {
                self.stack.set_visible_child_name("main");
                self.login.set_busy(false);
                let threading = self.store.borrow().threading;
                *self.store.borrow_mut() = Store::new(me, threading);
                // A read-only window must say so on its face. The whole
                // point is that it is pointed at somebody's real server.
                let mode = if std::env::var_os("MMRS_READ_ONLY").is_some() {
                    "  ·  read-only"
                } else {
                    ""
                };
                // Without the scheme: it is the same for every server and
                // it was pushing the read-only marker off the end.
                let host = server
                    .trim_end_matches('/')
                    .rsplit_once("//")
                    .map_or(server.as_str(), |(_, host)| host);
                self.identity
                    .set_text(&format!("{username} · {host}{mode}"));
                self.set_status("connected");
            }
            Event::Threading(threading) => {
                self.store.borrow_mut().threading = threading;
            }
            Event::Posts { channel, page } => {
                self.store
                    .borrow_mut()
                    .apply_post_list(&channel, &page, PageAnchor::Newest);
                if self.selected.borrow().as_ref() == Some(&channel) {
                    self.refresh_posts(&channel);
                }
            }
            Event::Ws(ws) => {
                self.maybe_notify(&ws);
                let changes = self.store.borrow_mut().apply_ws(&ws);
                if changes.is_empty() {
                    return;
                }
                self.refresh_sidebar();
                if let Some(open) = self.selected.borrow().clone() {
                    self.refresh_posts(&open);
                }
                // A reply to the open thread arrives as an ordinary posted
                // event, and with collapsed threads it is in no channel
                // timeline -- so the thread pane is the only place it can
                // appear at all.
                self.refresh_thread();
            }
            Event::ResyncRequired(why) => {
                self.set_status(&format!("reconnected: {why}"));
                self.commands.send(Command::Sync).ok();
            }
            Event::Status(text) => self.set_status(&text),
            Event::Failed(text) => self.set_status(&format!("error: {text}")),
            // Reference data arriving, which only ever means "store it and
            // redraw what it touches".
            other => self.on_data(other),
        }
    }

    /// Decide whether an incoming event deserves a desktop notification.
    ///
    /// Called before the store is updated, on purpose: "was I already looking
    /// at this channel?" has to be answered against the state as it was when
    /// the message arrived.
    fn maybe_notify(&self, event: &mm_api::WsEvent) {
        let mm_api::WsEvent::Posted {
            post,
            sender_name,
            mentions,
            ..
        } = event
        else {
            return;
        };

        let store = self.store.borrow();
        let me = store.me.clone();
        let mentioned = mentions.iter().any(|id| *id == me.as_str());
        let channel_name = store
            .channels
            .get(&post.channel_id)
            .map(|c| store.channel_label(c))
            .unwrap_or_default();
        let author = if sender_name.is_empty() {
            store.user_label(&post.user_id)
        } else {
            sender_name.trim_start_matches('@').to_owned()
        };

        let member = store.my_member.get(&post.channel_id);
        let settings = mm_desktop::NotifySettings {
            channel: member.map_or(mm_desktop::DesktopNotify::Inherit, |m| {
                mm_desktop::DesktopNotify::from_prop(
                    m.notify_props.get("desktop").map_or("", String::as_str),
                )
            }),
            muted: store.is_muted(&post.channel_id),
            ..mm_desktop::NotifySettings::default()
        };
        let focused = self.selected.borrow().clone();
        drop(store);

        if let Ok(notification) = mm_desktop::notification_for(
            post,
            &me,
            &author,
            &channel_name,
            mentioned,
            focused.as_ref(),
            &settings,
        ) {
            self.commands.send(Command::Notify(notification)).ok();
        }
    }

    fn refresh_sidebar(&self) {
        let sections = self.store.borrow().sidebar_sections();
        // The faces the sidebar needs are not necessarily the ones the open
        // channel needed, so ask here too -- once per person, as ever.
        for row in sections.iter().flat_map(|s| &s.rows) {
            let Some(user) = &row.avatar_user else {
                continue;
            };
            if !self.avatars.borrow().contains_key(user)
                && self.asked.borrow_mut().insert(user.clone())
            {
                let version = self
                    .store
                    .borrow()
                    .users
                    .get(user)
                    .map_or(mm_types::Timestamp::ZERO, |u| u.last_picture_update);
                self.commands
                    .send(Command::FetchAvatar {
                        user: user.clone(),
                        version,
                    })
                    .ok();
            }
        }
        let keep = self.selected.borrow().clone();

        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }

        let mut to_select = None;
        for section in &sections {
            self.sidebar.append(&section_header(section));
            for row in &section.rows {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                line.append(&self.channel_mark(row));
                let name = gtk::Label::builder()
                    .label(&row.display_name)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                if row.is_unread() {
                    name.add_css_class("heading");
                } else {
                    name.add_css_class("dim-label");
                }
                line.append(&name);

                if row.unread.mentions > 0 {
                    let badge = gtk::Label::builder()
                        .label(row.unread.mentions.to_string())
                        .css_classes(["accent", "caption-heading"])
                        .build();
                    line.append(&badge);
                } else if row.is_unread() && row.unread.messages > 0 {
                    let dot = gtk::Label::builder()
                        .label("●")
                        .css_classes(["dim-label", "caption"])
                        .build();
                    line.append(&dot);
                }
                if row.muted {
                    line.append(&gtk::Image::from_icon_name("audio-volume-muted-symbolic"));
                }

                let list_row = gtk::ListBoxRow::builder().child(&line).build();
                // The channel id travels on the widget name, so selection does not
                // need a parallel index that could drift from the sidebar.
                list_row.set_widget_name(row.channel.as_str());
                self.sidebar.append(&list_row);
                if keep.as_ref() == Some(&row.channel) {
                    to_select = Some(list_row);
                }
            }
        }
        match to_select {
            Some(row) => self.sidebar.select_row(Some(&row)),
            // Nothing selected yet: open the first channel rather than
            // leaving the message pane blank. The sidebar is sorted unread
            // first, so this lands on whatever most wants attention.
            None if keep.is_none() => {
                // Row 0 is a section heading now, and headings are not
                // selectable: walk to the first row that is.
                let first = (0..)
                    .map_while(|i| self.sidebar.row_at_index(i))
                    .find(gtk::ListBoxRow::is_selectable);
                if let Some(first) = first {
                    self.sidebar.select_row(Some(&first));
                }
            }
            None => {}
        }
    }

    /// Turn one post into the row the list draws, for either list.
    ///
    /// Shared so the thread pane and the channel cannot drift into
    /// rendering the same message two different ways.
    fn row_for(&self, store: &Store, post: &mm_types::Post, grouped: bool) -> PostRow {
        // A webhook posting under its own name must not borrow the face of
        // the account it posts through: an alert from "Alertmanager" showing
        // your own photograph is worse than showing an initial.
        // A join notice is the server talking, not the person it is about,
        // so it borrows nobody's face.
        let system = post.is_system();
        let overridden = post.override_username().is_some() || system;
        let avatar = if overridden {
            None
        } else {
            self.avatars.borrow().get(&post.user_id).cloned()
        };
        if avatar.is_none()
            && !overridden
            && !post.user_id.as_str().is_empty()
            && self.asked.borrow_mut().insert(post.user_id.clone())
        {
            self.commands
                .send(Command::FetchAvatar {
                    user: post.user_id.clone(),
                    version: store
                        .users
                        .get(&post.user_id)
                        .map_or(mm_types::Timestamp::ZERO, |u| u.last_picture_update),
                })
                .ok();
        }
        let (mut author, bot) = store.post_author(post);
        // Mattermost names a system notice "System" rather than leaving it
        // blank, which read as a message from nobody.
        if system {
            "System".clone_into(&mut author);
        }
        let mut doc = mm_md::parse(&post.message);
        mm_md::mark_mentions(&mut doc, &|name| store.mentioned(name));
        let mut blocks = render_blocks(&doc);
        // "(edited)" belongs on the end of the last thing said, not on a
        // line of its own.
        if post.is_edited() {
            match blocks.last_mut() {
                Some(last) if last.kind != BlockKind::Code => {
                    if last.markup {
                        last.text.push_str(&mm_md::escape(" (edited)"));
                    } else {
                        last.text.push_str(" (edited)");
                    }
                }
                // After a code block, on its own line: appending it inside
                // the box would read as part of the code.
                _ => blocks.push(BlockView {
                    kind: BlockKind::Text,
                    text: " (edited)".to_owned(),
                    markup: false,
                    quoted: false,
                    cells: Vec::new(),
                }),
            }
        }
        let images = self.images_for(post);
        let replies = store.reply_count(&post.id);
        PostRow::new(PostRowFields {
            id: post.id.as_str(),
            author: &author,
            body: "",
            blocks,
            time: &clock_time(post.create_at.millis()),
            meta: &describe_extras(post),
            flags: crate::row::PostRowFlags {
                grouped,
                voice: if system {
                    crate::row::Voice::System
                } else if bot {
                    crate::row::Voice::Bot
                } else {
                    crate::row::Voice::Person
                },
            },
            reactions: store.reactions_for(&post.id),
            avatar,
            images,
            cards: post
                .message_attachments()
                .iter()
                .map(|a| render_card(a, store))
                .collect(),
            // A reply never opens a thread of its own: in Mattermost the
            // whole conversation hangs off the root, and offering "2
            // replies" on a reply would open its parent under a different
            // name.
            replies: if post.is_reply() { 0 } else { replies },
        })
    }

    /// The images attached to a post, at the size they will be drawn.
    ///
    /// Every image gets a texture immediately, decoded from the
    /// `mini_preview` thumbnail that already rides in the post's metadata —
    /// no request, so the row is the right size from the first frame and
    /// never reflows when the real image lands on top of it.
    fn images_for(&self, post: &mm_types::Post) -> Vec<Attachment> {
        let mut out = Vec::new();
        for file in post.attachments() {
            if !file.is_image() {
                continue;
            }
            let Some((width, height)) = file.fitted(IMAGE_MAX_WIDTH, IMAGE_MAX_HEIGHT) else {
                // No dimensions means no space can be reserved, and an image
                // that reflows the list when it arrives is worse than the
                // attachment line the meta row already shows.
                continue;
            };

            let held = self.images.borrow().get(&file.id).cloned();
            let texture = if let Some(texture) = held {
                Some(texture)
            } else {
                {
                    // Ask for the real one, once.
                    if self.asked_files.borrow_mut().insert(file.id.clone()) {
                        self.commands
                            .send(Command::FetchFile {
                                file: file.id.clone(),
                                preview: file.has_preview_image,
                            })
                            .ok();
                    }
                    let placeholder = decode_mini_preview(&file.mini_preview);
                    if let Some(texture) = &placeholder {
                        self.images
                            .borrow_mut()
                            .insert(file.id.clone(), texture.clone());
                    }
                    placeholder
                }
            };
            out.push(Attachment {
                texture,
                width,
                height,
            });
        }
        out
    }

    /// Draw the open thread, or hide the panel when there is none.
    fn refresh_thread(&self) {
        let Some(root) = self.thread_root.borrow().clone() else {
            self.thread_pane.set_visible(false);
            return;
        };
        let store = self.store.borrow();
        self.thread_posts.remove_all();
        // No date separators here: a thread is one conversation and the
        // separators would say the same day over and over. Grouping still
        // applies, so a burst of replies from one person reads as one.
        let posts = store.thread_posts(&root);
        let mut previous: Option<&mm_types::Post> = None;
        for post in &posts {
            let grouped = previous.is_some_and(|p| {
                p.user_id == post.user_id
                    && !p.is_system()
                    && !post.is_system()
                    && post.create_at.millis() - p.create_at.millis() <= 5 * 60 * 1000
            });
            self.thread_posts
                .append(&self.row_for(&store, post, grouped));
            previous = Some(post);
        }
        drop(store);

        // Place the divider the first time the pane appears, and leave it
        // alone afterwards: once the reader has dragged it, it is theirs.
        if !self.thread_pane.is_visible() {
            self.thread_pane.set_visible(true);
            let available = self.conversation.width();
            if available > THREAD_PANE_WIDTH {
                self.conversation
                    .set_position(available - THREAD_PANE_WIDTH);
            }
        }
        let n = self.thread_posts.n_items();
        if n > 0 {
            let view = self.thread_view.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                view.scroll_to(n - 1, gtk::ListScrollFlags::empty(), None);
            });
        }
    }

    /// Take in reference data -- teams, channels, members, categories,
    /// the roster, a thread, a page of history, an image -- and redraw
    /// whatever it affects.
    ///
    /// Split from `on_event` so that function stays about the states a
    /// window can be in, and this stays about data arriving.
    fn on_data(&self, event: Event) {
        match event {
            Event::Teams(teams) => self.store.borrow_mut().apply_teams(teams),
            Event::Channels(channels) => {
                self.store.borrow_mut().apply_channels(channels);
                self.refresh_sidebar();
            }
            Event::Members(members) => {
                self.store.borrow_mut().apply_members(members);
                self.refresh_sidebar();
            }
            Event::Thread { root, page } => {
                let _ = self.store.borrow_mut().apply_thread(&page);
                *self.thread_root.borrow_mut() = Some(root.clone());
                self.refresh_thread();
            }
            Event::OlderPosts { channel, page } => self.on_older_posts(&channel, &page),
            Event::Avatar { user, image } => self.on_avatar(user, image),
            Event::File { file, image } => self.on_file(file, image),
            Event::NameFormat(format) => {
                self.store.borrow_mut().name_format = format;
                self.redraw_everything();
            }
            Event::Stats(stats) => {
                let channel = stats.channel_id.clone();
                self.store.borrow_mut().apply_stats(stats);
                if self.selected.borrow().as_ref() == Some(&channel) {
                    self.refresh_heading(&channel);
                }
            }
            Event::Categories(list) => {
                self.store.borrow_mut().apply_categories(&list);
                self.refresh_sidebar();
            }
            Event::Users(users) => {
                self.store.borrow_mut().apply_users(users);
                self.refresh_sidebar();
                // The roster arrives after the posts that needed it -- the
                // authors of a page are fetched from the page itself -- so
                // every row by a newly-known author is still showing a raw
                // user id until this redraw.
                if let Some(open) = self.selected.borrow().clone() {
                    self.refresh_posts(&open);
                }
            }
            // Every other variant has its own arm in `on_event`.
            _ => {}
        }
    }

    /// Merge a page of history fetched because the reader reached the top.
    fn on_older_posts(&self, channel: &ChannelId, page: &mm_types::PostList) {
        self.loading_older.set(false);
        self.store
            .borrow_mut()
            .apply_post_list(channel, page, PageAnchor::OlderThanHeld);
        if self.selected.borrow().as_ref() == Some(channel) {
            self.refresh_posts(channel);
        }
    }

    /// An attached image arrived: decode it over whatever placeholder the
    /// row is showing.
    fn on_file(&self, file: mm_types::FileId, image: Vec<u8>) {
        let bytes = glib::Bytes::from_owned(image);
        let Ok(texture) = gdk::Texture::from_bytes(&bytes) else {
            return;
        };
        self.images.borrow_mut().insert(file, texture);
        if let Some(open) = self.selected.borrow().clone() {
            self.refresh_posts(&open);
        }
        self.refresh_thread();
    }

    /// Redraw both lists and the sidebar, for a change that affects all of
    /// them -- a setting, rather than one message.
    fn redraw_everything(&self) {
        self.refresh_sidebar();
        if let Some(open) = self.selected.borrow().clone() {
            self.refresh_posts(&open);
        }
        self.refresh_thread();
    }

    /// Decode a face and redraw wherever it appears.
    ///
    /// `Texture::from_bytes` decodes on this thread. The plan puts image
    /// decode on the engine, and for message images it will stay there -- an
    /// avatar is at most 128px and decodes in well under a millisecond,
    /// which is cheaper than the round trip that moving it would cost.
    fn on_avatar(&self, user: UserId, image: Vec<u8>) {
        let bytes = glib::Bytes::from_owned(image);
        let Ok(texture) = gdk::Texture::from_bytes(&bytes) else {
            return;
        };
        self.avatars.borrow_mut().insert(user, texture);
        self.refresh_sidebar();
        if let Some(open) = self.selected.borrow().clone() {
            self.refresh_posts(&open);
        }
    }

    /// Show a line of status, and only while it says something.
    ///
    /// The window used to carry a permanent "ready" under the messages,
    /// which is a developer's line: it tells the user what they can already
    /// see and takes up room doing it. Anything that is not the quiet steady
    /// state -- loading, reconnecting, an error -- still shows.
    fn set_status(&self, text: &str) {
        const QUIET: [&str; 2] = ["ready", "connected"];
        self.status.set_text(text);
        self.status.set_visible(!QUIET.contains(&text));
    }

    /// What a sidebar row shows to its left: the other person's face for a
    /// DM, and the symbol for the channel's kind otherwise.
    ///
    /// Mattermost marks public channels with a hash and private ones with a
    /// padlock, and that distinction matters -- it is the difference between
    /// writing in front of the company and writing in front of six people.
    fn channel_mark(&self, row: &mm_core::SidebarRow) -> gtk::Widget {
        if let Some(user) = &row.avatar_user {
            let face = adw::Avatar::builder()
                .size(20)
                .show_initials(true)
                .text(&row.display_name)
                .build();
            face.set_custom_image(self.avatars.borrow().get(user));
            return face.upcast();
        }
        // Icon names are not checked at build time: a name the theme does
        // not have renders as a broken-image placeholder, which is how the
        // first attempt here -- Mattermost's own `channel-public-symbolic`
        // -- looked. These three are in Adwaita on this machine. A public
        // channel is a hash rather than an icon because that is the symbol
        // Mattermost uses and there is no Adwaita equivalent worth the
        // approximation.
        match row.kind {
            mm_types::ChannelType::Private => gtk::Image::builder()
                .icon_name("changes-prevent-symbolic")
                .css_classes(["dim-label"])
                .build()
                .upcast(),
            // A group DM is a conversation, not a channel.
            mm_types::ChannelType::Group => gtk::Image::builder()
                .icon_name("system-users-symbolic")
                .css_classes(["dim-label"])
                .build()
                .upcast(),
            _ => gtk::Label::builder()
                .label("#")
                .width_request(16)
                .css_classes(["dim-label"])
                .build()
                .upcast(),
        }
    }

    /// Put the open channel's name, and how many people are in it, in the
    /// window's title bar.
    fn refresh_heading(&self, channel: &ChannelId) {
        let (name, members) = self.store.borrow().channel_heading(channel);
        self.title.set_title(&name);
        self.title.set_subtitle(&match members {
            // "1 member" would be the user alone in a channel, which is a
            // real state and reads badly in the plural.
            Some(1) => "1 member".to_owned(),
            Some(n) => format!("{n} members"),
            None => String::new(),
        });
    }

    fn refresh_posts(&self, channel: &ChannelId) {
        self.refresh_heading(channel);
        // Was the newest message in view before this update? Asked of the
        // model, not of the scroll adjustment: `upper` is an estimate GTK
        // revises as rows are realized, and it was observed moving 20% in both
        // directions inside one run. See docs/gtk-notes.md.
        let was_at_end = self.posts.n_items() == 0 || self.stick_to_bottom.get();

        let store = self.store.borrow();
        self.posts.remove_all();
        // Date separators and author grouping are computed in mm-core, where
        // they are covered: they are calendar arithmetic and identity
        // comparisons, and both are wrong in ways only a test notices.
        let visible = store.channel_posts(channel);
        let offset = local_utc_offset_secs();
        let unread_from = *self.unread_from.borrow();
        for row in mm_core::rows::project(&visible, offset, unread_from) {
            let (id, grouped) = match row {
                mm_core::Row::Date { at, date } => {
                    self.posts
                        .append(&PostRow::separator(&describe_day(at, &date)));
                    continue;
                }
                mm_core::Row::NewMessages => {
                    self.posts.append(&PostRow::new_messages());
                    continue;
                }
                mm_core::Row::Post { id, grouped } => (id, grouped),
            };
            let Some(post) = store.posts.get(&id) else {
                continue;
            };
            self.posts.append(&self.row_for(&store, post, grouped));
        }
        self.composer.set_sensitive(true);

        // No bottom alignment. Resting a short channel's messages on the
        // composer looked better, but `valign: End` makes ListView ask for
        // its *natural* height, and for a virtualised list that is an
        // estimate which can exceed the real content. The surplus then
        // shows as a blank gap above the first message -- in a real channel
        // nearly three hundred pixels of it, which is far worse than the
        // empty pane it was meant to fix.

        // A page of history was just prepended: put the reader back on the
        // message they were resting on, rather than at the top of sixty
        // newly loaded ones. This takes precedence over following the tail,
        // and the two cannot both apply -- reaching the top edge means they
        // had already scrolled away from the bottom.
        if let Some(anchor) = self.restore_to.borrow_mut().take() {
            let at = (0..self.posts.n_items())
                .filter_map(|i| self.posts.item(i).and_downcast::<PostRow>().map(|r| (i, r)))
                .find(|(_, row)| row.id() == anchor.as_str())
                .map(|(i, _)| i);
            if let Some(at) = at {
                let view = self.post_view.clone();
                // The same timer as the initial scroll, for the same reason:
                // ListView has not measured the rows it was just handed.
                glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                    view.scroll_to(at, gtk::ListScrollFlags::empty(), None);
                });
                return;
            }
        }

        if was_at_end {
            self.stick_to_bottom.set(true);
            let n = self.posts.n_items();
            if n > 0 {
                // A timer, not an idle, and not adjustment arithmetic.
                //
                // Both of those were tried and neither worked: ListView has
                // not measured the rows it was just handed, so an immediate or
                // idle `scroll_to` lands short, and pinning the adjustment
                // from its own `changed` handler does not take. What the
                // week-one spike actually measured was that `scroll_to`
                // succeeds once a layout pass has happened, and its pass came
                // from a timer. See docs/gtk-notes.md.
                //
                // The adjustment handlers in `wire` still earn their place:
                // they follow messages arriving *later*, when the list is
                // already measured.
                let view = self.post_view.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                    view.scroll_to(n - 1, gtk::ListScrollFlags::empty(), None);
                });
            }
        }
    }
}
