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
use mm_types::{ChannelId, FileId, PostId, UserId};
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

/// Cloneable: every field is a `Rc` or a GTK object, both of which are
/// handles to one shared thing, so a clone is another handle on the same
/// window rather than a second window. It is what lets a deferred callback
/// hold the window without borrowing it.
#[derive(Clone)]
struct Ui {
    store: Rc<RefCell<Store>>,
    /// The server's base URL, for building a link to a message.
    server: Rc<RefCell<String>>,
    /// Swaps between the sign-in screen and the chat view. The window starts
    /// on sign-in and only moves once the engine reports a session, so there
    /// is never a moment where an empty chat view is shown as though it were
    /// loading.
    stack: gtk::Stack,
    login: Rc<LoginPage>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    selected: Rc<RefCell<Option<ChannelId>>>,
    /// Which of the three views the middle of the window is showing.
    pane: Rc<RefCell<Pane>>,
    middle: gtk::Stack,
    threads_list: gtk::ListBox,
    /// Whether the Threads view is filtered to the unread ones, which is
    /// the "Unreads" tab Mattermost puts beside "Followed threads".
    unreads_only: Rc<std::cell::Cell<bool>>,
    /// Whether the sidebar is filtered to the channels wanting
    /// attention. The open channel is exempt, for the reason a folded
    /// category keeps it: hiding where the reader is standing is never
    /// what they asked for.
    sidebar_unread_only: Rc<std::cell::Cell<bool>>,
    unreads_tab: gtk::ToggleButton,
    drafts_list: gtk::ListBox,
    sidebar: gtk::ListBox,
    identity: gtk::Label,
    /// Decoded avatars, by user. A texture is a GPU-side object and must be
    /// built on this thread, so the engine sends bytes and the decode
    /// happens here.
    avatars: Rc<RefCell<HashMap<UserId, gdk::Texture>>>,
    /// Who has typed in a channel lately, and when they last did.
    ///
    /// View state, not store state: it expires on a clock, and the store
    /// is deliberately clock-free so that replaying events is exact. An
    /// entry older than `TYPING_LIFETIME` is simply not shown.
    typing: Typing,
    typing_line: gtk::Label,
    /// Pictures fetched for link previews, shared with the post rows that
    /// draw them.
    link_images: LinkImages,
    /// Decoded images attached to messages, by file id. Seeded from the
    /// `mini_preview` thumbnail that already rides in the metadata, then
    /// replaced by the real image when it arrives.
    images: Rc<RefCell<HashMap<mm_types::FileId, gdk::Texture>>>,
    /// The image window that is open, if one is: which file it shows and
    /// the picture to put a better copy into when one arrives.
    viewer: Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
    /// The thread pane: its model, its view, the panel itself and which
    /// root it is showing.
    thread_posts: gio::ListStore,
    thread_view: gtk::ListView,
    thread_pane: gtk::Box,
    thread_scroller: gtk::ScrolledWindow,
    /// Whether the thread pane should follow its newest reply.
    ///
    /// The same rule the channel list follows, and for the same reason:
    /// without it every redraw scrolled the pane to the bottom, so
    /// reacting to a message half way up a long thread threw the reader
    /// to the end of it.
    thread_stick: Rc<std::cell::Cell<bool>>,
    conversation: gtk::Paned,
    thread_root: Rc<RefCell<Option<PostId>>>,
    /// Where the reader was when they opened the channel, for the "new
    /// messages" line. Captured before marking read, because marking read
    /// moves it.
    unread_from: Rc<RefCell<Option<mm_types::Timestamp>>>,
    /// The sidebar as it was last drawn.
    ///
    /// The sidebar is rebuilt from scratch, which destroys every row. Do
    /// that while somebody is clicking one and the click lands on a widget
    /// that no longer exists and nothing happens -- which is what made
    /// channels seem not to open while a channel's avatars were still
    /// arriving. Rebuilt only when it would actually look different.
    drawn_sidebar: Rc<RefCell<Option<Vec<mm_core::SidebarSection>>>>,
    /// An avatar arrived, which changes how the sidebar looks without
    /// changing any of the rows it is built from.
    sidebar_stale: Rc<std::cell::Cell<bool>>,
    /// Whether a redraw is already queued.
    ///
    /// Avatars and images arrive one at a time, and each one used to redraw
    /// the whole channel -- sixty rows, every message re-parsed. Twenty
    /// images meant twenty rebuilds, and opening a busy channel took so
    /// long that the previous channel's messages stayed on screen for
    /// seconds after the click. They are collected into one redraw instead.
    redraw_queued: Rc<std::cell::Cell<bool>>,
    sidebar_queued: Rc<std::cell::Cell<bool>>,
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
        // No `Command::Sync` here. The engine syncs as soon as it has a
        // session, and asking for a second one ran every request twice --
        // two sets of answers, and the window drew the channel again for
        // each of them.
        let ui = Rc::new(build(app, &commands));

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

/// The one hover toolbar currently on screen, so the next one can take
/// it down.
///
/// Leave events alone are not enough: `ListView` recycles a row widget
/// under a motionless pointer, so a row that was hovered can be rebound
/// to a different message without ever being left, and its toolbar stays
/// up over somebody else's words. Both post lists share one of these, so
/// the thread pane cannot leave one showing behind the channel either.
type ShownTools = Rc<RefCell<Option<gtk::Box>>>;

/// Pictures fetched for link previews, keyed by the URL they came from.
///
/// A present key means the picture has been asked for, so a row drawn
/// again -- and a recycled row is drawn constantly -- does not ask a
/// second time. `None` is "asked, not here yet".
type LinkImages = Rc<RefCell<HashMap<String, Option<gdk::Texture>>>>;

/// Who has typed in each channel lately, and when they last did.
type Typing = Rc<RefCell<HashMap<ChannelId, Vec<(UserId, std::time::Instant)>>>>;

/// The thread panel: a post list, a heading with a close button, and a
/// composer that replies into the open thread.
///
/// Hidden until a thread is opened — an empty panel permanently taking a
/// third of the window would be worse than no panel.
fn build_thread_pane(
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
    server: &Rc<RefCell<String>>,
    viewer: &Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
    shown: &ShownTools,
    links: &LinkImages,
) -> (
    gio::ListStore,
    gtk::ListView,
    gtk::Box,
    gtk::Button,
    gtk::Entry,
    gtk::ScrolledWindow,
) {
    let (thread_posts, thread_view, thread_scroller) =
        build_post_list(commands, store, server, viewer, shown, links, true);
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
        thread_scroller,
    )
}

/// The channel list, under a line naming the account this window shows.
fn build_sidebar() -> (gtk::ListBox, gtk::Label, gtk::ToggleButton, gtk::Box) {
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
    // Mattermost puts a filter at the top of the channel list that hides
    // everything caught up. On a busy account that is the difference
    // between a sidebar you read and one you scroll.
    let unread_only = gtk::ToggleButton::builder()
        .icon_name("object-select-symbolic")
        .tooltip_text("Show unread channels only")
        .css_classes(["flat"])
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    identity.set_hexpand(true);
    top.append(&identity);
    top.append(&unread_only);
    top.set_margin_end(6);

    let sidebar_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_pane.set_size_request(260, -1);
    sidebar_pane.append(&top);
    sidebar_pane.append(&sidebar_scroll);
    (sidebar, identity, unread_only, sidebar_pane)
}

/// Styling this window needs that the theme does not provide.
///
/// Links, because GTK paints them in the desktop's accent colour -- orange
/// on Ubuntu -- and a chat client showing orange links beside a Mattermost
/// client showing blue ones reads as a different application. The colour is
/// Mattermost's own and is legible on both themes.
///
/// And the hover toolbar, which floats over a message and so needs a
/// background of its own: without one its buttons are drawn on top of the
/// text they belong to and neither can be read.
fn install_styles() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        "link { color: #5d89ea; } link:hover { color: #86a9f2; }\n\
         .mmrs-tools { \
             background: @popover_bg_color; \
             border: 1px solid @borders; \
             border-radius: 6px; \
             padding: 1px; \
         }\n\
         .mmrs-destructive label { color: @destructive_color; }\n\
         .mmrs-chip { \
             background: alpha(currentColor, 0.1); \
             border-radius: 4px; \
             padding: 0 6px; \
         }\n\
         .mmrs-dot { \
             min-width: 7px; \
             min-height: 7px; \
             border-radius: 9999px; \
             border: 2px solid @view_bg_color; \
             background: @insensitive_fg_color; \
         }\n\
         .mmrs-dot.online { background: #3db887; }\n\
         .mmrs-dot.away { background: #ffbc1f; }\n\
         .mmrs-dot.dnd { background: #d24b4e; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// The window itself: its title bar, the sign-in page, and the stack
/// that swaps between that page and the chat view.
fn build_window(
    app: &adw::Application,
    split: &gtk::Paned,
) -> (
    adw::WindowTitle,
    gtk::Button,
    Rc<LoginPage>,
    gtk::Stack,
    adw::ApplicationWindow,
) {
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
    stack.add_named(split, Some("main"));
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

    (title, refresh, login, stack, window)
}

fn build(app: &adw::Application, commands: &tokio::sync::mpsc::UnboundedSender<Command>) -> Ui {
    install_styles();
    let (sidebar, identity, unread_only, sidebar_pane) = build_sidebar();

    // Made here rather than in the `Ui` below, because the post list's
    // widgets are built before it and the copy-link button needs both: a
    // permalink is the server, the team and the post, and only the store
    // knows the last two.
    let store = Rc::new(RefCell::new(Store::new(UserId::default(), Threading::Flat)));
    let server = Rc::new(RefCell::new(String::new()));

    let viewer: Rc<RefCell<Option<(FileId, gtk::Picture)>>> = Rc::new(RefCell::new(None));
    let shown_tools: ShownTools = Rc::new(RefCell::new(None));
    let link_images: LinkImages = Rc::new(RefCell::new(HashMap::new()));
    let (posts, post_view, scroller) = build_post_list(
        commands,
        &store,
        &server,
        &viewer,
        &shown_tools,
        &link_images,
        false,
    );

    let (composer, status) = build_composer_row();

    // The conversation, the Threads view and the Drafts view take turns
    // in the same space. The status line and the composer stay put under
    // all three -- the composer insensitive for the two that have nothing
    // to send to.
    let (threads_list, drafts_list, unreads_tab, middle) = build_middle(&scroller);

    let typing_line = build_typing_line();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&middle);
    content.append(&typing_line);
    content.append(&status);
    content.append(&composer);

    let (thread_posts, thread_view, thread_pane, thread_close, thread_composer, thread_scroller) =
        build_thread_pane(
            commands,
            &store,
            &server,
            &viewer,
            &shown_tools,
            &link_images,
        );

    let (conversation, split) = build_panes(&sidebar_pane, &content, &thread_pane);

    let (title, refresh, login, stack, window) = build_window(app, &split);

    let ui = Ui {
        store,
        server,
        commands: commands.clone(),
        selected: Rc::new(RefCell::new(None)),
        pane: Rc::new(RefCell::new(Pane::Channel)),
        middle: middle.clone(),
        threads_list: threads_list.clone(),
        unreads_only: Rc::new(std::cell::Cell::new(false)),
        sidebar_unread_only: Rc::new(std::cell::Cell::new(false)),
        unreads_tab: unreads_tab.clone(),
        drafts_list: drafts_list.clone(),
        sidebar: sidebar.clone(),
        identity: identity.clone(),
        thread_posts: thread_posts.clone(),
        thread_view: thread_view.clone(),
        thread_pane: thread_pane.clone(),
        thread_scroller: thread_scroller.clone(),
        thread_stick: Rc::new(std::cell::Cell::new(true)),
        conversation: conversation.clone(),
        thread_root: Rc::new(RefCell::new(None)),
        drawn_sidebar: Rc::new(RefCell::new(None)),
        sidebar_stale: Rc::new(std::cell::Cell::new(false)),
        redraw_queued: Rc::new(std::cell::Cell::new(false)),
        sidebar_queued: Rc::new(std::cell::Cell::new(false)),
        unread_from: Rc::new(RefCell::new(None)),
        loading_older: Rc::new(std::cell::Cell::new(false)),
        restore_to: Rc::new(RefCell::new(None)),
        avatars: Rc::new(RefCell::new(HashMap::new())),
        typing: Rc::new(RefCell::new(HashMap::new())),
        typing_line,
        link_images: Rc::clone(&link_images),
        images: Rc::new(RefCell::new(HashMap::new())),
        viewer: Rc::clone(&viewer),
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
    wire_views(&ui);
    wire_switcher(&ui, &window);
    wire_unread_filter(&ui, &unread_only);
    attach_both_composers(&ui, &composer, &thread_composer);

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
    // Scrolling up means the reader is reading, not following. Arriving
    // back at the bottom opts them back in. The same pair the channel
    // list uses, and the same reason it does not ask the adjustment:
    // `upper` is an estimate `ListView` revises as rows are realized.
    {
        let stick = Rc::clone(&ui.thread_stick);
        let scrolling = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scrolling.connect_scroll(move |_, _, dy| {
            if dy < 0.0 {
                stick.set(false);
            }
            glib::Propagation::Proceed
        });
        ui.thread_scroller.add_controller(scrolling);
    }
    {
        let stick = Rc::clone(&ui.thread_stick);
        ui.thread_scroller.connect_edge_reached(move |_, edge| {
            if edge == gtk::PositionType::Bottom {
                stick.set(true);
            }
        });
    }
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
    let store = Rc::clone(&ui.store);
    composer.connect_activate(move |entry| {
        let text = entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(root) = root.borrow().clone() else {
            return;
        };
        // The channel comes from the root post, never from the sidebar's
        // selection. A reply carries both, and they have to agree: the
        // server checks that the root really is in the channel and
        // answers 400 when it is not.
        //
        // They stopped agreeing the moment a thread could be opened from
        // the Threads view, which opens a conversation without changing
        // which channel is selected -- so every reply sent from there
        // was addressed to whatever channel happened to be open and was
        // refused. Asking the post is right either way.
        let Some(channel) = store
            .borrow()
            .posts
            .get(&root)
            .map(|post| post.channel_id.clone())
        else {
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
/// A sidebar entry for one of the two views, drawn like a channel so
/// the list reads as one list.
fn view_entry(name: &str, icon: &str, label: &str, count: i64, dot: bool) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(&gtk::Image::from_icon_name(icon));
    line.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(if count > 0 {
                ["heading"]
            } else {
                ["dim-label"]
            })
            .build(),
    );
    if count > 0 {
        line.append(
            &gtk::Label::builder()
                .label(count.to_string())
                .css_classes(["accent", "caption-heading"])
                .build(),
        );
    } else if dot {
        line.append(
            &gtk::Label::builder()
                .label("●")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.set_widget_name(name);
    row
}

/// What a view says when it has nothing in it. Better than a blank
/// pane, which reads as something that failed to load.
fn empty_view(what: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(what)
        .margin_top(24)
        .css_classes(["dim-label"])
        .build();
    gtk::ListBoxRow::builder()
        .child(&label)
        .selectable(false)
        .activatable(false)
        .build()
}

/// The three things that take turns in the middle of the window: the
/// conversation, the Threads view and the Drafts view.
fn build_middle(
    scroller: &gtk::ScrolledWindow,
) -> (gtk::ListBox, gtk::ListBox, gtk::ToggleButton, gtk::Stack) {
    let (threads_list, threads_scroll) = build_view_list();
    let (unreads_tab, threads_page) = build_threads_page(&threads_scroll);
    let (drafts_list, drafts_page) = build_view_list();
    let middle = gtk::Stack::new();
    middle.add_named(scroller, Some("channel"));
    middle.add_named(&threads_page, Some("threads"));
    middle.add_named(&drafts_page, Some("drafts"));
    middle.set_vexpand(true);
    (threads_list, drafts_list, unreads_tab, middle)
}

/// The Threads view's two tabs over its list, the way Mattermost puts
/// them: everything followed, or only what has new replies.
fn build_threads_page(list: &gtk::ScrolledWindow) -> (gtk::ToggleButton, gtk::Box) {
    let followed = gtk::ToggleButton::builder()
        .label("Followed threads")
        .active(true)
        .css_classes(["flat"])
        .build();
    let unreads = gtk::ToggleButton::builder()
        .label("Unreads")
        .css_classes(["flat"])
        .group(&followed)
        .build();

    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    tabs.set_margin_start(8);
    tabs.set_margin_end(8);
    tabs.set_margin_top(6);
    tabs.set_margin_bottom(2);
    tabs.append(&followed);
    tabs.append(&unreads);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&tabs);
    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(list);
    // Only the Unreads button is returned: grouping the two makes them a
    // radio pair, so "followed" is simply "unreads off".
    (unreads, page)
}

/// A list for one of the two views: the Threads list and the Drafts
/// list are the same furniture with different rows in them.
fn build_view_list() -> (gtk::ListBox, gtk::ScrolledWindow) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["navigation-sidebar"])
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        // Never horizontally, for the reason the message list gives: a
        // long line has to wrap rather than run off the edge.
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    (list, scroll)
}

/// One row in the Threads or the Drafts view.
///
/// Laid out the way Mattermost lays it out: who and where on the first
/// line with how long ago on the right, two lines of the message under
/// it, and a footer -- the faces of the people in the thread, then the
/// reply count.
struct ViewRow<'a> {
    /// Bold, first. The author for a thread; the channel for a draft,
    /// which has no author but the reader.
    who: &'a str,
    /// The grey pill after the name. Empty for a draft, which has said
    /// where it is already.
    chip: &'a str,
    ago: &'a str,
    preview: &'a str,
    /// The line under the message: "28 replies", "a reply".
    footer: &'a str,
    faces: Vec<Option<gdk::Texture>>,
    /// Names for the faces, so one with no picture yet draws initials
    /// rather than an empty circle.
    face_names: Vec<String>,
    unread: bool,
}

fn view_row(row: &ViewRow<'_>) -> gtk::ListBoxRow {
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.append(
        &gtk::Label::builder()
            .label(row.who)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    if !row.chip.is_empty() {
        top.append(
            &gtk::Label::builder()
                .label(row.chip)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption", "dim-label", "mmrs-chip"])
                .build(),
        );
    }
    // Pushes the timestamp to the right-hand edge.
    top.append(&gtk::Box::builder().hexpand(true).build());
    if row.unread {
        top.append(
            &gtk::Label::builder()
                .label("\u{25cf}")
                .css_classes(["accent", "caption"])
                .build(),
        );
    }
    top.append(
        &gtk::Label::builder()
            .label(row.ago)
            .css_classes(["dim-label", "caption"])
            .build(),
    );

    // Two lines of the message and no more: the view is a list of things
    // to go to, not a place to read them.
    let body = gtk::Label::builder()
        .label(row.preview)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column.set_margin_top(10);
    column.set_margin_bottom(10);
    column.append(&top);
    column.append(&body);

    if !row.footer.is_empty() || !row.faces.is_empty() {
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        // The faces of the people in the thread, as Mattermost shows
        // them: who is in this conversation is most of how a reader
        // recognises which conversation it is.
        for (face, name) in row.faces.iter().zip(&row.face_names) {
            let avatar = adw::Avatar::new(20, Some(name), true);
            avatar.set_custom_image(face.as_ref());
            foot.append(&avatar);
        }
        if !row.footer.is_empty() {
            foot.append(
                &gtk::Label::builder()
                    .label(row.footer)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
        }
        column.append(&foot);
    }

    gtk::ListBoxRow::builder().child(&column).build()
}

/// Connect the two views: a row in either is a place to go.
fn wire_views(ui: &Ui) {
    {
        let ui = ui.clone();
        ui.unreads_tab.clone().connect_toggled(move |tab| {
            ui.unreads_only.set(tab.is_active());
            ui.refresh_threads_view();
        });
    }
    {
        let ui = ui.clone();
        // The row's name is the root post, and opening a thread is what
        // the thread pane is already for.
        ui.threads_list
            .clone()
            .connect_row_activated(move |_, row| {
                let root = PostId::new(row.widget_name().as_str());
                if root.as_str().is_empty() {
                    return;
                }
                ui.commands.send(Command::OpenThread(root)).ok();
            });
    }
    {
        let ui = ui.clone();
        // A draft's row is named for the channel it is waiting in, and
        // going to a draft means going to that channel.
        ui.drafts_list.clone().connect_row_activated(move |_, row| {
            let channel = ChannelId::new(row.widget_name().as_str());
            if channel.as_str().is_empty() {
                return;
            }
            ui.show_channel(&channel);
        });
    }
}

/// The switcher's keys: Escape closes, Return goes, Up and Down move
/// the selection.
///
/// Capture phase, for the reason Ctrl+K needs it: the entry has the
/// focus, and a `GtkSearchEntry` takes Return, Up and Down for itself
/// before a controller on the window would ever see them.
fn switcher_keys(
    window: &adw::Window,
    list: &gtk::ListBox,
    go: impl Fn(&gtk::ListBoxRow) + 'static,
) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    let window = window.clone();
    let list = list.clone();
    keys.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Escape => {
            window.close();
            glib::Propagation::Stop
        }
        // Return acts on the selection even though the caret is in the
        // entry, which is where somebody typing expects it to be.
        gdk::Key::Return | gdk::Key::KP_Enter => {
            if let Some(row) = list.selected_row() {
                go(&row);
            }
            glib::Propagation::Stop
        }
        gdk::Key::Down | gdk::Key::Up => {
            let at = list.selected_row().map_or(0, |r| r.index());
            let next = if key == gdk::Key::Down {
                at + 1
            } else {
                at - 1
            };
            if let Some(row) = list.row_at_index(next.max(0)) {
                list.select_row(Some(&row));
            }
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys
}

/// The sidebar's unread filter, which Mattermost puts at the top of the
/// channel list: on a busy account it is the difference between a
/// sidebar somebody reads and one they scroll.
fn wire_unread_filter(ui: &Ui, button: &gtk::ToggleButton) {
    let ui = ui.clone();
    button.connect_toggled(move |button| {
        ui.sidebar_unread_only.set(button.is_active());
        // The filter is not part of the sections the sidebar compares
        // against, so say outright that what it draws has changed.
        ui.sidebar_stale.set(true);
        ui.refresh_sidebar();
    });
}

/// The `@` the caret is sitting at the end of, if it is.
///
/// Only a mention being typed *now*: the run has to reach the caret,
/// start at a word boundary -- an address has an `@` in the middle and
/// is not a mention -- and carry no space, since a mention is one word.
fn mention_being_typed(text: &str, caret: usize) -> Option<(usize, String)> {
    let upto: String = text.chars().take(caret).collect();
    let at = upto.rfind('@')?;
    let before = upto[..at].chars().next_back();
    if before.is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let prefix = &upto[at + 1..];
    if prefix.chars().any(char::is_whitespace) {
        return None;
    }
    Some((upto[..at].chars().count(), prefix.to_owned()))
}

/// The sidebar, the conversation and the thread panel, side by side.
///
/// Two nested `Paned` rather than one `OverlaySplitView` each side: the
/// thread panel keeps its width when the window is resized, and the
/// sidebar keeps its own, which is what the reader set them to.
fn build_panes(
    sidebar: &gtk::Box,
    content: &gtk::Box,
    thread: &gtk::Box,
) -> (gtk::Paned, gtk::Paned) {
    let conversation = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(content)
        .end_child(thread)
        .resize_end_child(false)
        .shrink_end_child(false)
        .build();
    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(sidebar)
        .end_child(&conversation)
        .resize_start_child(false)
        .position(260)
        .build();
    (conversation, split)
}

/// A face with a presence dot in its bottom corner.
///
/// An overlay rather than a badge: `adw::Avatar` has no notion of one,
/// and a dot drawn as a sibling would take width from the message.
fn face_with_dot(size: i32) -> (gtk::Overlay, adw::Avatar, gtk::Box) {
    let face = adw::Avatar::builder()
        .size(size)
        .show_initials(true)
        .valign(gtk::Align::Start)
        .build();
    let dot = gtk::Box::builder()
        .css_classes(["mmrs-dot"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .visible(false)
        .build();
    let gutter = gtk::Overlay::builder().child(&face).build();
    gutter.add_overlay(&dot);
    gutter.set_valign(gtk::Align::Start);
    (gutter, face, dot)
}

/// Colour the dot, or hide it for somebody who is not there.
///
/// Offline is drawn as nothing rather than a grey dot: Mattermost does
/// the same, and a column of grey dots beside every old message is
/// noise about people who are simply not at work.
fn set_presence(dot: &gtk::Box, presence: mm_types::Presence) {
    for state in mm_types::Presence::ALL {
        dot.remove_css_class(state.as_str());
    }
    if presence == mm_types::Presence::Offline {
        dot.set_visible(false);
        return;
    }
    dot.add_css_class(presence.as_str());
    dot.set_tooltip_text(Some(presence.label()));
    dot.set_visible(true);
}

/// The box messages are typed into, and the status line above it.
///
/// Insensitive until a channel is open: a composer that accepts text
/// with nowhere to send it is a message the reader will lose.
fn build_composer_row() -> (gtk::Entry, gtk::Label) {
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
    (composer, status)
}

/// The "somebody is typing" line, between the last message and the box
/// being typed into, which is where Mattermost puts it.
///
/// Hidden when nobody is, so it takes no height from the conversation
/// and the messages do not shift up and down as people type.
fn build_typing_line() -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .margin_start(14)
        .margin_bottom(2)
        .visible(false)
        .css_classes(["dim-label", "caption"])
        .build()
}

/// Give both composers their `@` popup.
///
/// Each is told which conversation it is in, so the names it offers are
/// that channel's members. The thread's channel comes from its root
/// post, never from whatever is open behind it -- the same mistake that
/// once sent replies to the wrong channel.
fn attach_both_composers(ui: &Ui, composer: &gtk::Entry, thread_composer: &gtk::Entry) {
    attach_mentions(ui, composer, |ui| ui.selected.borrow().clone());
    attach_mentions(ui, thread_composer, |ui| {
        let root = ui.thread_root.borrow().clone()?;
        let store = ui.store.borrow();
        store.posts.get(&root).map(|post| post.channel_id.clone())
    });
}

/// Offer the people a half-typed `@` might mean, and complete it.
///
/// Both composers get one: a mention is as likely in a reply as in a
/// message, and Mattermost offers it in both.
fn attach_mentions(
    ui: &Ui,
    entry: &gtk::Entry,
    channel: impl Fn(&Ui) -> Option<ChannelId> + 'static,
) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Browse)
        .css_classes(["navigation-sidebar"])
        .build();
    // Sized to what is in it, not to the space available: a popover
    // over one name should be one name tall.
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(MENTIONS_HEIGHT)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_size_request(MENTIONS_WIDTH, -1);
    column.append(&scroll);

    let popup = gtk::Popover::builder()
        .child(&column)
        .autohide(false)
        .position(gtk::PositionType::Top)
        .has_arrow(false)
        .build();
    popup.set_parent(entry);

    {
        let ui = ui.clone();
        let list = list.clone();
        let popup = popup.clone();
        entry.connect_changed(move |entry| {
            let caret = usize::try_from(entry.position().max(0)).unwrap_or(0);
            let Some((_, prefix)) = mention_being_typed(entry.text().as_str(), caret) else {
                popup.popdown();
                return;
            };
            fill_mentions(&ui, &list, &prefix, channel(&ui).as_ref());
            if list.row_at_index(0).is_none() {
                popup.popdown();
            } else {
                popup.popup();
            }
        });
    }
    {
        let entry = entry.clone();
        let popup = popup.clone();
        list.connect_row_activated(move |_, row| {
            accept_mention(&entry, &row.widget_name());
            popup.popdown();
        });
    }
    entry.add_controller(mention_keys(entry, &list, &popup));
}

/// The popup's keys, while it is up: move, accept, dismiss.
///
/// Capture phase, like the switcher's: a `GtkEntry` with the focus
/// takes Up, Down, Tab and Return for itself, and the caret is in one.
/// Everything is passed through when the popup is down, so typing is
/// untouched.
fn mention_keys(
    entry: &gtk::Entry,
    list: &gtk::ListBox,
    popup: &gtk::Popover,
) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    let entry = entry.clone();
    let list = list.clone();
    let popup = popup.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if !popup.is_visible() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Escape => {
                popup.popdown();
                glib::Propagation::Stop
            }
            gdk::Key::Tab | gdk::Key::Return | gdk::Key::KP_Enter => {
                if let Some(row) = list.selected_row() {
                    accept_mention(&entry, &row.widget_name());
                }
                popup.popdown();
                glib::Propagation::Stop
            }
            gdk::Key::Down | gdk::Key::Up => {
                let at = list.selected_row().map_or(0, |r| r.index());
                let step = if key == gdk::Key::Down { 1 } else { -1 };
                if let Some(row) = first_selectable(&list, at + step, step) {
                    list.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys
}

/// Draw the people who match, with the name beside the username the way
/// Mattermost does: the username is what the mention becomes, the name
/// is what the reader was thinking of.
fn fill_mentions(ui: &Ui, list: &gtk::ListBox, prefix: &str, channel: Option<&ChannelId>) {
    while let Some(old) = list.first_child() {
        list.remove(&old);
    }
    let store = ui.store.borrow();
    let mut heading_drawn = (false, false);
    for found in store.mention_candidates(channel, prefix, MENTIONS_OFFERED) {
        // Two sections, like Mattermost's: the people in this
        // conversation, then anyone else the typing matches. The second
        // heading only appears once somebody is under it.
        let drawn = if found.in_channel {
            &mut heading_drawn.0
        } else {
            &mut heading_drawn.1
        };
        if !*drawn {
            *drawn = true;
            let text = if found.in_channel {
                "CHANNEL MEMBERS"
            } else {
                "NOT IN CHANNEL"
            };
            list.append(&mention_heading(text));
        }
        list.append(&mention_row(ui, found.user));
    }
    if let Some(first) = first_selectable(list, 0, 1) {
        list.select_row(Some(&first));
    }
}

/// A section label, as a row that cannot be selected or activated so the
/// arrow keys and a click both pass over it.
fn mention_heading(text: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_start(12)
        .margin_top(6)
        .margin_bottom(2)
        .css_classes(["dim-label", "caption-heading"])
        .build();
    let row = gtk::ListBoxRow::builder().child(&label).build();
    row.set_selectable(false);
    row.set_activatable(false);
    row
}

/// One person, with the name beside the username the way Mattermost
/// does: the username is what the mention becomes, the name is what the
/// reader was thinking of.
fn mention_row(ui: &Ui, user: &mm_types::User) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_margin_start(12);
    line.set_margin_end(12);
    line.set_margin_top(2);
    line.set_margin_bottom(2);
    let face = adw::Avatar::new(20, Some(&user.username), true);
    face.set_custom_image(ui.avatars.borrow().get(&user.id));
    line.append(&face);
    line.append(
        &gtk::Label::builder()
            .label(format!("@{}", user.username))
            .css_classes(["heading"])
            .build(),
    );
    let human = format!("{} {}", user.first_name, user.last_name);
    if !human.trim().is_empty() {
        line.append(
            &gtk::Label::builder()
                .label(human.trim())
                .css_classes(["dim-label"])
                .build(),
        );
    }
    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.set_widget_name(&user.username);
    row
}

/// The first row at or after `from` that can hold the selection, walking
/// in `step`'s direction. The section headings sit in the same list, so
/// stepping by one would land the selection on a label.
fn first_selectable(list: &gtk::ListBox, from: i32, step: i32) -> Option<gtk::ListBoxRow> {
    let mut at = from;
    while at >= 0 {
        let row = list.row_at_index(at)?;
        if row.is_selectable() {
            return Some(row);
        }
        at += step;
    }
    None
}

/// Put the chosen username where the half-typed one was, and a space
/// after it, which is what somebody mid-sentence wants next.
fn accept_mention(entry: &gtk::Entry, username: &str) {
    let caret = usize::try_from(entry.position().max(0)).unwrap_or(0);
    let text = entry.text().to_string();
    let Some((at, prefix)) = mention_being_typed(&text, caret) else {
        return;
    };
    let head: String = text.chars().take(at).collect();
    let tail: String = text.chars().skip(at + 1 + prefix.chars().count()).collect();
    let filled = format!("{head}@{username} {tail}");
    entry.set_text(&filled);
    let put = at + username.chars().count() + 2;
    entry.set_position(i32::try_from(put).unwrap_or(i32::MAX));
}

/// Ctrl+K, the way Mattermost binds the quick switcher.
///
/// On the window rather than on a widget, so it works wherever the
/// focus is -- including inside the composer, which is where a reader's
/// caret usually is when they decide to go somewhere else.
fn wire_switcher(ui: &Ui, window: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    let ui = ui.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key == gdk::Key::k && state.contains(gdk::ModifierType::CONTROL_MASK) {
            ui.open_switcher();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    // Capture, not bubble: a `GtkEntry` with the focus consumes key
    // presses, and the composer has the focus most of the time.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    window.add_controller(keys);
}

/// Connect folding a category shut and opening it again.
fn wire_categories(ui: &Ui, sidebar: &gtk::ListBox) {
    let ui = ui.clone();
    sidebar.connect_row_activated(move |_, row| {
        // Channel rows are activatable too -- that is how a click opens
        // one -- so this has to say which kind it was handed.
        if row.has_css_class("mmrs-section") {
            ui.toggle_category(&row.widget_name());
        }
    });
}

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
    let ui_for_views = ui.clone();
    sidebar.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let name = row.widget_name();
        // The two views sit in the same list as the channels and are
        // selected the same way, but they open something else.
        if name == THREADS_ROW {
            ui_for_views.show_pane(&Pane::Threads);
            ui_for_views.set_title("Threads", "");
            return;
        }
        if name == DRAFTS_ROW {
            ui_for_views.show_pane(&Pane::Drafts);
            ui_for_views.set_title("Drafts", "");
            return;
        }
        let id = ChannelId::new(name.to_string());

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
        ui_for_views.show_pane(&Pane::Channel);
        // Draw what is already held, before asking for anything. A
        // channel read earlier, or warmed in the background, is in the
        // store already -- and waiting for the round trip to redraw it
        // was most of what opening a channel felt like. The answer,
        // when it comes, splices in only what differs.
        ui_for_views.refresh_posts(&id);
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
    wire_categories(ui, sidebar);

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

/// How long ago something happened, the way Mattermost writes it in
/// these two views: "3 minutes ago", "23 hours ago", "yesterday", "4
/// days ago", and a date once it is far enough back to stop counting.
///
/// Relative rather than a clock time because the list spans weeks. A
/// column of "14:48 / 12:42 / 23 Oct" says nothing about which of them
/// is the one that just moved, which is the only question the view is
/// there to answer.
fn describe_ago(at: mm_types::Timestamp) -> String {
    use chrono::{Local, TimeZone};

    let chrono::offset::LocalResult::Single(dt) = Local.timestamp_millis_opt(at.millis()) else {
        return String::new();
    };
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    let since = Local::now().signed_duration_since(dt);
    // A clock skew between this machine and the server puts a message a
    // few seconds in the future; "in 4 seconds" would be absurd.
    let minutes = since.num_minutes().max(0);
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return plural(minutes, "minute");
    }
    let hours = since.num_hours();
    if hours < 24 {
        return plural(hours, "hour");
    }
    let days = since.num_days();
    if days == 1 {
        return "yesterday".to_owned();
    }
    if days < 7 {
        return plural(days, "day");
    }
    // Mattermost counts in weeks for about a month before giving up and
    // printing a date, and says "last week" rather than "1 week ago".
    if days < 14 {
        return "last week".to_owned();
    }
    if days < 31 {
        return plural(days / 7, "week");
    }
    dt.format("%-d %b").to_string()
}

/// The whole moment, spelled out, for the tooltip on a message's time.
///
/// Mattermost writes it "October 15 at 17:01:35", and the seconds are
/// there on purpose: two messages a moment apart show the same clock
/// time, and this is where a reader goes to tell them apart.
fn full_time(millis: i64) -> String {
    use chrono::{Local, TimeZone};
    if let chrono::offset::LocalResult::Single(dt) = Local.timestamp_millis_opt(millis) {
        dt.format("%B %-d, %Y at %H:%M:%S").to_string()
    } else {
        String::new()
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
/// The day boundary, drawn as a rule with the date sitting in it.
///
/// Every row carries one and all but the first of each day hide it,
/// because `ListView` recycles a fixed set of widgets and a row that could
/// not become a separator would need a second factory.
fn build_day_separator() -> gtk::Box {
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
    day
}

/// The toolbar that appears over a message on hover.
///
/// Built once per recycled row and relabelled on bind, because `ListView`
/// reuses a small set of rows and a button made per bind would be a button
/// made per scroll. Which post it acts on is read from the row's widget
/// name at click time, for the same reason.
fn build_message_tools(
    row: &gtk::Box,
    item: &gtk::ListItem,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
    server: &Rc<RefCell<String>>,
    shown: &ShownTools,
) -> gtk::Box {
    // The hover toolbar: the one-click reactions this reader uses most,
    // then save and reply, then the overflow menu. Mattermost reveals
    // these on hover rather than drawing them on every message, and a
    // channel with a toolbar on every row is unreadable.
    //
    // The quick-reaction buttons are built once, here, and relabelled on
    // bind: `ListView` recycles rows, so a button made per bind would be a
    // button made per scroll.
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.add_css_class("linked");
    tools.add_css_class("mmrs-tools");
    tools.set_margin_top(2);
    tools.set_margin_end(6);
    tools.set_halign(gtk::Align::End);
    tools.set_valign(gtk::Align::Start);
    tools.set_visible(false);

    for slot in 0..QUICK_REACTIONS {
        tools.append(&quick_reaction_button(slot, row, commands, store));
    }

    let reply = gtk::Button::from_icon_name("mail-reply-sender-symbolic");
    reply.add_css_class("flat");
    reply.set_tooltip_text(Some("Reply in thread"));
    tools.append(&reply);

    // Saving, which Mattermost puts in the toolbar as well as the menu.
    // Copy-link moved the other way, into the menu, where Mattermost has
    // it: a toolbar of six buttons over a message is a lot of furniture.
    let saver = gtk::Button::from_icon_name("bookmark-new-symbolic");
    saver.add_css_class("flat");
    {
        let commands = commands.clone();
        let store = Rc::clone(store);
        let row = row.clone();
        saver.connect_clicked(move |_| {
            let post = row.widget_name();
            if post.is_empty() {
                return;
            }
            let post = PostId::new(post.as_str());
            let already = store.borrow().is_saved(&post);
            commands
                .send(Command::Save {
                    post,
                    saved: !already,
                })
                .ok();
        });
    }
    tools.append(&saver);

    // Everything else, behind the "…" the Electron client uses. Built as
    // a plain popover of buttons rather than a `GMenu`: the entries change
    // per message -- pin or unpin, save or unsave, delete only your own --
    // and a menu model would have to be rebuilt anyway.
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .css_classes(["flat"])
        .tooltip_text("More actions")
        .build();
    tools.append(&more);

    {
        // The menu is made, and filled, the first time it is opened.
        // `set_create_popup_func` is GTK's own hook for exactly that, and
        // it runs before the menu is popped up, so the entries are laid
        // out in time for the first frame.
        //
        // Not a micro-optimisation. A `GtkPopover` is a `GtkNative`, and
        // constructing one came to about eleven milliseconds -- ninety-
        // eight per cent of what making a row cost. Drawing a channel of
        // sixty messages took 2.3 seconds, nearly all of it menus for
        // messages nobody had clicked. Filling it here rather than on
        // bind saves as much again: the entries depend on the message --
        // pin or unpin, save or unsave, delete only your own -- so they
        // were rebuilt for every row of every redraw. See docs/gtk-notes.md.
        let commands = commands.clone();
        let store = Rc::clone(store);
        let server = Rc::clone(server);
        let item = item.clone();
        let tools_for_close = tools.clone();
        let row_for_close = row.clone();
        more.set_create_popup_func(move |more| {
            let popover = more.popover().unwrap_or_else(|| {
                let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
                actions.set_margin_top(4);
                actions.set_margin_bottom(4);
                let popover = gtk::Popover::builder().child(&actions).build();
                // Hide the toolbar once the menu closes, since by then the
                // pointer has usually moved on and no leave is coming --
                // unless it is still on the message, in which case the
                // toolbar belongs where it is. `closed` lives on the
                // popover, not on the button.
                let tools = tools_for_close.clone();
                let row = row_for_close.clone();
                popover.connect_closed(move |_| {
                    if !row.state_flags().contains(gtk::StateFlags::PRELIGHT) {
                        tools.set_visible(false);
                    }
                });
                more.set_popover(Some(&popover));
                popover
            });
            if let Some(data) = item.item().and_downcast::<PostRow>() {
                fill_message_menu(&popover, &data, &commands, &store, &server);
            }
        });
    }

    let hover = gtk::EventControllerMotion::new();
    {
        let tools = tools.clone();
        // Showing it is `first_hover`'s job, in `setup_post_row`; this
        // only takes it away again.
        // Not while its own menu is open. Opening the popover sends the
        // pointer a leave, which hid the toolbar, which took the popover
        // down with it -- the menu appeared and vanished in one frame.
        let more = more.clone();
        let shown = Rc::clone(shown);
        hover.connect_leave(move |_| {
            if !more.is_active() {
                tools.set_visible(false);
                let mut shown = shown.borrow_mut();
                if shown.as_ref() == Some(&tools) {
                    *shown = None;
                }
            }
        });
    }
    row.add_controller(hover);

    tools
}

fn setup_post_row(
    item: &gtk::ListItem,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
    server: &Rc<RefCell<String>>,
    shown: &ShownTools,
) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(4);
    row.set_margin_bottom(4);

    let day = build_day_separator();

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
    let (gutter, _, _) = face_with_dot(32);
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
    line.append(&gutter);
    line.append(&main);

    row.append(&day);
    // The reply button floats over the message rather than sitting beside
    // it: a child of the row would take width from the text, and the text
    // is what the reader came for.
    let floating = gtk::Overlay::new();
    floating.set_child(Some(&line));
    row.append(&floating);

    // The hover toolbar is made the first time the pointer reaches this
    // row, and never for a row nobody hovers over.
    //
    // Measured, not guessed: handing `ListView` a hundred and two rows
    // took 2411 ms with the toolbar built here and 145 ms without it --
    // the toolbar is ninety-four per cent of what drawing a channel
    // costs, and it is invisible until the pointer arrives. It is not
    // any one widget in it either: taking only the popover out changed
    // nothing. See docs/gtk-notes.md.
    let first_hover = gtk::EventControllerMotion::new();
    {
        let row = row.clone();
        let floating = floating.clone();
        let item = item.clone();
        let commands = commands.clone();
        let store = Rc::clone(store);
        let server = Rc::clone(server);
        let shown = Rc::clone(shown);
        first_hover.connect_enter(move |_, _, _| {
            // The overlay's last child is the toolbar once there is one;
            // before that it is the message itself.
            let tools = match floating.last_child().and_downcast::<gtk::Box>() {
                Some(tools) if tools.has_css_class("mmrs-tools") => tools,
                _ => {
                    let tools =
                        build_message_tools(&row, &item, &commands, &store, &server, &shown);
                    floating.add_overlay(&tools);
                    tools
                }
            };
            // A day separator has nothing to act on, and the row under
            // the pointer may have been recycled since its toolbar was
            // made.
            if let Some(data) = item.item().and_downcast::<PostRow>() {
                if data.is_separator() {
                    return;
                }
                label_quick_reactions(&tools, &data);
                tools.set_visible(true);
                show_only(&shown, &tools);
            }
        });
    }
    row.add_controller(first_hover);

    item.set_child(Some(&row));
}

/// Make this the only toolbar on screen.
///
/// The leave event is the usual way one goes away, but it does not
/// always come: `ListView` can recycle the row under a still pointer,
/// and a popover grabbing the pointer sends a leave that is deliberately
/// ignored. One cell holding whichever toolbar is up makes the invariant
/// -- at most one -- true by construction rather than by hoping every
/// crossing arrives.
fn show_only(shown: &ShownTools, tools: &gtk::Box) {
    let previous = shown.borrow_mut().replace(tools.clone());
    if let Some(previous) = previous
        && previous != *tools
    {
        previous.set_visible(false);
    }
}

/// Take down the toolbar of a row that is about to show a different
/// message.
fn hide_tools(item: &gtk::ListItem, shown: &ShownTools) {
    let Some(tools) = item
        .child()
        .and_downcast::<gtk::Box>()
        .and_then(|row| row.last_child())
        .and_downcast::<gtk::Overlay>()
        .and_then(|floating| floating.last_child())
        .and_downcast::<gtk::Box>()
        .filter(|tools| tools.has_css_class("mmrs-tools"))
    else {
        return;
    };
    tools.set_visible(false);
    let mut shown = shown.borrow_mut();
    if shown.as_ref() == Some(&tools) {
        *shown = None;
    }
}

/// Put this message's one-click reactions on the toolbar's buttons.
///
/// The buttons are made once and reused as rows are recycled, so the
/// emoji is put on them here; the tooltip carries the shortcode, which is
/// what the click handler sends.
fn label_quick_reactions(tools: &gtk::Box, data: &PostRow) {
    let mine: Vec<String> = data
        .reactions()
        .into_iter()
        .filter(|r| r.mine)
        .map(|r| r.emoji)
        .collect();
    let mut button = tools.first_child();
    for emoji in data.quick_reactions() {
        let Some(current) = button.and_downcast::<gtk::Button>() else {
            break;
        };
        if !current.widget_name().starts_with("quick") {
            break;
        }
        current.set_label(
            &mm_md::glyph(&emoji).map_or_else(|| format!(":{emoji}:"), ToOwned::to_owned),
        );
        current.set_tooltip_text(Some(&emoji));
        // Lit when the reader has already used it, so the button says
        // which way it will go before it is pressed -- the same signal
        // the reaction chips under the message give.
        if mine.contains(&emoji) {
            current.add_css_class("suggested-action");
        } else {
            current.remove_css_class("suggested-action");
        }
        button = current.next_sibling();
    }
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
fn bind_cards(
    cards: &gtk::Box,
    data: &PostRow,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    links: &LinkImages,
) {
    while let Some(old) = cards.first_child() {
        cards.remove(&old);
    }
    let views = data.cards();
    let previews = data.previews();
    cards.set_visible(!views.is_empty() || !previews.is_empty());

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

    for preview in previews {
        cards.append(&preview_card(&preview, commands, links));
    }
}

/// What the server found behind a link: the site, the title as a link,
/// the description, and the picture the page offered.
///
/// Mattermost draws this under the message rather than replacing the
/// URL in it, so the link the author typed stays where they put it.
fn preview_card(
    preview: &mm_types::LinkPreview,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    links: &LinkImages,
) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_hexpand(true);
    if !preview.site_name.is_empty() {
        column.append(&card_label(
            &preview.site_name,
            false,
            &["dim-label", "caption"],
        ));
    }
    if !preview.title.is_empty() {
        let markup = format!(
            "<a href=\"{}\">{}</a>",
            mm_md::escape(&preview.url),
            mm_md::escape(&preview.title)
        );
        column.append(&card_label(&markup, true, &["heading"]));
    }
    if !preview.description.is_empty() {
        column.append(&card_label(&preview.description, false, &["dim-label"]));
    }

    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("card");
    card.set_margin_start(10);
    card.set_margin_top(4);
    card.set_halign(gtk::Align::Start);
    card.set_size_request(PREVIEW_WIDTH, -1);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column.set_margin_top(10);
    column.set_margin_bottom(10);
    card.append(&column);

    if let Some(url) = &preview.image {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .valign(gtk::Align::Center)
            .margin_end(10)
            .build();
        picture.set_size_request(PREVIEW_IMAGE, PREVIEW_IMAGE);
        match links.borrow_mut().entry(url.clone()) {
            std::collections::hash_map::Entry::Occupied(held) => {
                picture.set_paintable(held.get().as_ref());
            }
            std::collections::hash_map::Entry::Vacant(empty) => {
                empty.insert(None);
                let _ = commands.send(Command::FetchLinkImage(url.clone()));
            }
        }
        card.append(&picture);
    }
    card.upcast()
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
            BlockKind::Table => {
                // Inside its own horizontal scroller. A table cell does not
                // wrap -- a column broken across lines is unreadable -- so
                // a wide table has a wide *minimum* width, and every row in
                // a ListView shares the widest row's allocation. One wide
                // table in one message therefore pushed every other
                // message's text off the right-hand edge.
                let scroller = gtk::ScrolledWindow::builder()
                    .child(&build_table(&block.cells))
                    .hscrollbar_policy(gtk::PolicyType::Automatic)
                    .vscrollbar_policy(gtk::PolicyType::Never)
                    .propagate_natural_width(true)
                    .propagate_natural_height(true)
                    .build();
                scroller.upcast()
            }
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

fn bind_images(
    images: &gtk::Box,
    data: &PostRow,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    viewer: &Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
) {
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

        // A click opens it full size, which is what a thumbnail is for.
        let click = gtk::GestureClick::new();
        {
            let image = image.clone();
            let commands = commands.clone();
            let viewer = Rc::clone(viewer);
            click.connect_released(move |_, _, _, _| {
                open_image(&image, &commands, &viewer);
            });
        }
        picture.add_controller(click);
        // So the pointer says it is something to click.
        picture.set_cursor_from_name(Some("pointer"));

        images.append(&picture);
    }
}

/// One of the hover toolbar's one-click reactions.
///
/// Made once and relabelled on bind, because rows are recycled: a button
/// made per bind would be a button made per scroll. Which emoji it
/// carries is therefore read at click time, off the button itself.
fn quick_reaction_button(
    slot: usize,
    row: &gtk::Box,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
) -> gtk::Button {
    let button = gtk::Button::builder().css_classes(["flat"]).build();
    button.set_widget_name(&format!("quick{slot}"));
    let commands = commands.clone();
    let row = row.clone();
    let store = Rc::clone(store);
    button.connect_clicked(move |button| {
        // The emoji is on the button, put there at bind time; the post
        // is on the row, for the same reason.
        let (post, emoji) = (row.widget_name(), button.tooltip_text());
        let (false, Some(emoji)) = (post.is_empty(), emoji) else {
            return;
        };
        let post = PostId::new(post.as_str());
        // A toggle, not an add. Clicking a reaction you have already made
        // takes it back -- there is no separate way to undo one, here or
        // anywhere in Mattermost -- and asking the store is what tells
        // the two apart. It used to send `add: true` whatever the state,
        // so the one-click reactions could only ever be put on.
        let add = !store.borrow().reacted(&post, &emoji);
        commands
            .send(Command::React {
                post,
                emoji: emoji.to_string(),
                add,
            })
            .ok();
    });
    button
}

/// Open an attached image in a window of its own.
///
/// Mattermost opens a lightbox over the channel; this is a window,
/// because a client that is already a window manager's client has no
/// business drawing its own modal scrim. What matters is the same: the
/// image at the size the screen can give it rather than the size a
/// message row can.
///
/// The copy already in hand is shown at once -- it is the server's
/// preview, and waiting on the network to show something that is already
/// decoded would be a blank window for a second. The full-size one is
/// asked for at the same time and swapped in when it lands.
fn open_image(
    attachment: &Attachment,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    viewer: &Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
) {
    let Some(texture) = &attachment.texture else {
        // Nothing decoded yet: the row is still showing a placeholder,
        // and an empty window would say less than the row does.
        return;
    };
    let picture = gtk::Picture::builder()
        .paintable(texture)
        .content_fit(gtk::ContentFit::ScaleDown)
        .build();

    let window = adw::Window::builder()
        .title("Image")
        .default_width(IMAGE_WINDOW_WIDTH)
        .default_height(IMAGE_WINDOW_HEIGHT)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&adw::HeaderBar::new());
    layout.append(&picture);
    picture.set_vexpand(true);
    window.set_content(Some(&layout));

    // Escape, because this is a viewer and that is what closes one.
    let keys = gtk::EventControllerKey::new();
    {
        let window = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);

    // Only one viewer is tracked, which is the one that can still be
    // improved by bytes arriving; closing it takes the entry away so a
    // later file does not try to paint into a window that is gone.
    *viewer.borrow_mut() = Some((attachment.file.clone(), picture));
    {
        let viewer = Rc::clone(viewer);
        window.connect_close_request(move |_| {
            viewer.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }
    commands
        .send(Command::FetchFile {
            file: attachment.file.clone(),
            preview: false,
        })
        .ok();
    window.present();
}

/// Fill a message's overflow menu.
///
/// Rebuilt per bind because the entries depend on the message -- pin or
/// unpin, save or unsave, delete only your own -- and rows are recycled,
/// so the one built last is for a different post.
fn fill_message_menu(
    popover: &gtk::Popover,
    data: &PostRow,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
    server: &Rc<RefCell<String>>,
) {
    let Some(actions) = popover.child().and_downcast::<gtk::Box>() else {
        return;
    };
    while let Some(old) = actions.first_child() {
        actions.remove(&old);
    }

    let post = PostId::new(data.id());
    if post.as_str().is_empty() {
        return;
    }
    let (pinned, saved, mine) = {
        let store = store.borrow();
        (
            store.posts.get(&post).is_some_and(|p| p.is_pinned),
            store.is_saved(&post),
            store.is_mine(&post),
        )
    };

    // An icon, then a left-aligned label -- not a centred one. Mattermost
    // lays its menu out this way and a column of centred text reads as a
    // different application. The icon names are checked against the theme:
    // one it does not have draws a broken-image placeholder and logs
    // nothing.
    let entry = |icon: &str, label: &str| {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.append(&gtk::Image::from_icon_name(icon));
        line.append(
            &gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        gtk::Button::builder()
            .child(&line)
            .css_classes(["flat"])
            .build()
    };
    let add = |button: &gtk::Button, command: Command| {
        let commands = commands.clone();
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            commands.send(command.clone()).ok();
            popover.popdown();
        });
        actions.append(button);
    };
    let copies = |button: &gtk::Button, text: String| {
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&text);
            }
            popover.popdown();
        });
        actions.append(button);
    };

    add(
        &entry("mail-reply-sender-symbolic", "Reply in thread"),
        Command::OpenThread(post.clone()),
    );
    add(
        &entry("mail-unread-symbolic", "Mark as unread"),
        Command::MarkUnread(post.clone()),
    );
    // Both read "undo what is already true" when it is: there is no
    // separate unsave or unpin anywhere in Mattermost.
    let saving = if saved {
        "Remove from saved"
    } else {
        "Save message"
    };
    add(
        &entry("bookmark-new-symbolic", saving),
        Command::Save {
            post: post.clone(),
            saved: !saved,
        },
    );
    let pinning = if pinned {
        "Unpin from channel"
    } else {
        "Pin to channel"
    };
    add(
        &entry("view-pin-symbolic", pinning),
        Command::Pin {
            post: post.clone(),
            pinned: !pinned,
        },
    );

    // Copying is local, so it goes straight to the clipboard rather than
    // through the engine. Mattermost groups the two of them apart.
    actions.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    if let Some(address) = store.borrow().permalink(&server.borrow(), &post) {
        copies(&entry("insert-link-symbolic", "Copy link"), address);
    }
    copies(&entry("edit-copy-symbolic", "Copy text"), data.plain_body());

    if mine {
        actions.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let edit = entry("document-edit-symbolic", "Edit message");
        // What the author typed, not what is drawn: an edit starts from
        // the markdown they wrote.
        wire_edit(&edit, popover, &post, &data.plain_body(), commands);
        actions.append(&edit);

        let delete = entry("user-trash-symbolic", "Delete message");
        delete.add_css_class("mmrs-destructive");
        add(&delete, Command::Delete(post));
    }
}

/// Connect the menu's Edit entry to the window that does the editing.
fn wire_edit(
    edit: &gtk::Button,
    popover: &gtk::Popover,
    post: &PostId,
    was: &str,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    let commands = commands.clone();
    let popover = popover.clone();
    let post = post.clone();
    let was = was.to_owned();
    edit.connect_clicked(move |button| {
        popover.popdown();
        open_editor(button, &post, &was, &commands);
    });
}

/// Edit a message: the text as it was written, in a window of its own.
///
/// A window rather than editing in place. Mattermost turns the row into
/// a text box, which a `ListView` makes awkward -- the row is recycled
/// out from under the caret the moment anything else arrives -- and the
/// thing that matters is that the author sees the markdown they wrote
/// and can change it.
fn open_editor(
    near: &gtk::Button,
    post: &PostId,
    was: &str,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    view.buffer().set_text(was);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_end(12);
    buttons.set_margin_bottom(12);
    buttons.append(&cancel);
    buttons.append(&save);

    let window = adw::Window::builder()
        .title("Edit message")
        .modal(true)
        .default_width(EDITOR_WIDTH)
        .default_height(EDITOR_HEIGHT)
        .build();
    if let Some(parent) = near.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 8);
    layout.append(&adw::HeaderBar::new());
    layout.append(&scroll);
    layout.append(&buttons);
    window.set_content(Some(&layout));

    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    {
        let commands = commands.clone();
        let post = post.clone();
        let view = view.clone();
        let window = window.clone();
        let was = was.to_owned();
        save.connect_clicked(move |_| {
            let buffer = view.buffer();
            let (from, to) = buffer.bounds();
            let message = buffer.text(&from, &to, false).to_string();
            window.close();
            // An edit that changes nothing is a request the server does
            // not need, and it would stamp `edit_at` for no reason.
            if message.trim().is_empty() || message == was {
                return;
            }
            commands
                .send(Command::Edit {
                    post: post.clone(),
                    message,
                })
                .ok();
        });
    }

    window.present();
    view.grab_focus();
}

/// Hide the hover toolbar and relabel its one-click reactions for the row
/// now bound to it.
fn bind_message_tools(floating: &gtk::Overlay, data: &PostRow) {
    // Only a row somebody has hovered over has a toolbar at all, and one
    // that does is being recycled onto another message, so its one-click
    // reactions have to be relabelled. The overflow menu is made and
    // filled when it is opened, not here: its entries depend on the
    // message, so doing it per bind meant a menu for every row of every
    // redraw.
    if let Some(tools) = floating.last_child().and_downcast::<gtk::Box>()
        && tools.has_css_class("mmrs-tools")
    {
        tools.set_visible(false);
        label_quick_reactions(&tools, data);
    }
}

/// The widgets one recycled post row is made of.
///
/// Found by walking the tree `setup_post_row` built, by position rather
/// than by `first_child`/`last_child`: there are a dozen now, and
/// `last_child()` silently became the wrong one the day the meta line
/// was added. A missing one means the tree changed and this walk did
/// not, so every lookup is fallible and the row simply does not bind.
struct RowWidgets {
    row: gtk::Box,
    day: gtk::Box,
    floating: gtk::Overlay,
    line: gtk::Box,
    face: adw::Avatar,
    dot: gtk::Box,
    main: gtk::Box,
    header: gtk::Box,
    body: gtk::Box,
    cards: gtk::Box,
    images: gtk::Box,
    meta: gtk::Label,
    reactions: gtk::Box,
}

impl RowWidgets {
    fn find(item: &gtk::ListItem) -> Option<Self> {
        let row = item.child().and_downcast::<gtk::Box>()?;
        let day = row.first_child().and_downcast::<gtk::Box>()?;
        let floating = day.next_sibling().and_downcast::<gtk::Overlay>()?;
        let line = floating.child().and_downcast::<gtk::Box>()?;
        let gutter = line.first_child().and_downcast::<gtk::Overlay>()?;
        let face = gutter.child().and_downcast::<adw::Avatar>()?;
        let dot = gutter.last_child().and_downcast::<gtk::Box>()?;
        let main = gutter.next_sibling().and_downcast::<gtk::Box>()?;
        let header = main.first_child().and_downcast::<gtk::Box>()?;
        let body = header.next_sibling().and_downcast::<gtk::Box>()?;
        let cards = body.next_sibling().and_downcast::<gtk::Box>()?;
        let images = cards.next_sibling().and_downcast::<gtk::Box>()?;
        let meta = images.next_sibling().and_downcast::<gtk::Label>()?;
        let reactions = meta.next_sibling().and_downcast::<gtk::Box>()?;
        Some(Self {
            row,
            day,
            floating,
            line,
            face,
            dot,
            main,
            header,
            body,
            cards,
            images,
            meta,
            reactions,
        })
    }
}

fn bind_post_row(
    item: &gtk::ListItem,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    viewer: &Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
    links: &LinkImages,
    relative: bool,
) {
    let Some(data) = item.item().and_downcast::<PostRow>() else {
        return;
    };
    let Some(RowWidgets {
        row,
        day,
        floating,
        line,
        face,
        dot,
        main,
        header,
        body,
        cards,
        images,
        meta,
        reactions,
    }) = RowWidgets::find(item)
    else {
        return;
    };
    set_presence(&dot, data.presence());

    // A separator is a day label and nothing else.
    let is_separator = data.is_separator();
    day.set_visible(is_separator);
    if is_separator {
        bind_separator_row(&day, &line, &data);
        return;
    }

    line.set_visible(true);
    // The post id travels on the widget name, so the reply button -- which
    // is connected once, in setup, because these rows are recycled -- can
    // read it at click time without a handler per bind.
    row.set_widget_name(&data.id());
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
        let said = if relative { data.ago() } else { data.time() };
        time.set_text(&said);
        // The clock is short either way, so the whole date lives here.
        time.set_tooltip_text(Some(&data.full_time()));
    }

    fill_blocks(&body, &data.blocks());
    if system {
        body.add_css_class("dim-label");
    } else {
        body.remove_css_class("dim-label");
    }

    bind_message_tools(&floating, &data);

    bind_cards(&cards, &data, commands, links);
    bind_images(&images, &data, commands, viewer);

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

/// How big the edit window opens: a few lines of message, not a page.
const EDITOR_WIDTH: i32 = 560;
const EDITOR_HEIGHT: i32 = 260;

/// How many people the mention popup offers before the reader should
/// type another letter rather than scroll a list.
const MENTIONS_OFFERED: usize = 8;
/// How wide a link preview card is drawn, and how big a side the picture
/// in it gets. Fixed rather than fitted: a page's `OpenGraph` image can be
/// a 1200px banner, and a row that resizes itself when one arrives moves
/// everything the reader was looking at.
/// How long a `typing` event stands for before the line goes away.
///
/// The server re-sends while somebody keeps typing, throttled by
/// `TimeBetweenUserTypingUpdatesMilliseconds`, which defaults to five
/// seconds -- so the window has to be longer than that or the line
/// flickers off between a typist's own keystrokes.
const TYPING_LIFETIME: std::time::Duration = std::time::Duration::from_secs(7);
const PREVIEW_WIDTH: i32 = 520;
const PREVIEW_IMAGE: i32 = 80;
const MENTIONS_WIDTH: i32 = 340;
const MENTIONS_HEIGHT: i32 = 260;

/// The quick switcher's shape, and how many channels it offers before
/// the reader should type another letter rather than scroll.
const SWITCHER_WIDTH: i32 = 520;
const SWITCHER_HEIGHT: i32 = 460;
const SWITCHER_RESULTS: usize = 30;

/// How big the image window opens. Large enough to be worth opening,
/// small enough not to fill a 4K screen with somebody's screenshot.
const IMAGE_WINDOW_WIDTH: i32 = 1100;
const IMAGE_WINDOW_HEIGHT: i32 = 800;

/// How many of a thread's participants to draw. Mattermost shows a
/// handful and stops; a thread of forty people would otherwise be a row
/// of forty circles.
const FACES: usize = 5;

/// How many one-click reaction buttons a message's hover toolbar carries.
const QUICK_REACTIONS: usize = 3;

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

/// Which of the three things the middle of the window is showing.
///
/// Mattermost puts Threads and Drafts above the channel list and swaps
/// the conversation out for them, rather than opening them in a window
/// of their own, and a reader coming from that client expects the back
/// of the sidebar to work the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    Channel,
    Threads,
    Drafts,
}

/// What the sidebar rows for the two views are called, so a click can
/// tell them from a channel. A channel id is 26 base32 characters, so
/// neither can collide with one.
const THREADS_ROW: &str = "mmrs:threads";
const DRAFTS_ROW: &str = "mmrs:drafts";

/// How much of the window a thread takes when it is open.
const THREAD_PANE_WIDTH: i32 = 380;

/// A category heading: not selectable, so arrow keys and the first-channel
/// fallback walk straight past it.
fn section_header(section: &mm_core::SidebarSection) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.set_margin_top(10);
    line.set_margin_bottom(2);
    // The chevron Mattermost puts there, pointing down when the category
    // is open and along when it is folded.
    let chevron = gtk::Image::from_icon_name(if section.collapsed {
        "pan-end-symbolic"
    } else {
        "pan-down-symbolic"
    });
    chevron.add_css_class("dim-label");
    line.append(&chevron);
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

    // Activatable but not selectable: clicking it folds the category,
    // while arrow keys and the first-channel fallback walk straight past
    // it the way they always did.
    let row = gtk::ListBoxRow::builder()
        .child(&line)
        .selectable(false)
        .activatable(true)
        .build();
    row.add_css_class("mmrs-section");
    // Which category this heading is, for the click. A section invented
    // for channels the server filed in no category has no id and cannot
    // be folded; `Store::category` returns nothing for it.
    row.set_widget_name(&section.id);
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

/// Put `next` into `model`, replacing only the run that differs.
///
/// `ListView` builds a widget set for every row it is handed -- measured,
/// not assumed: a hundred-row channel realizes a hundred rows -- so
/// handing it the whole channel on every redraw made each redraw cost the
/// whole channel. An avatar landing, a message arriving, a page of history
/// loaded: each one redrew everything, and every page of history made the
/// next redraw worse, until the window stopped answering.
///
/// Two cases, because a message list only ever does two things. The list
/// grows at one end -- history prepended, a new message appended -- which
/// a common prefix and a common suffix find exactly. Or it stays the same
/// length and some rows are redrawn, which is every avatar, image,
/// reaction and edit, and those are replaced one at a time.
///
/// Returns whether anything moved.
fn splice_changed(model: &gio::ListStore, next: &[PostRow]) -> bool {
    let drawn: Vec<PostRow> = (0..model.n_items())
        .filter_map(|i| model.item(i).and_downcast::<PostRow>())
        .collect();

    // The same rows, some of them redrawn: replace those, one at a time.
    // This is the common case and the one a prefix-and-suffix diff is
    // worst at -- an avatar arriving changes every row that person wrote,
    // scattered down the channel, and the run from the first of them to
    // the last is most of it.
    if drawn.len() == next.len() {
        let mut changed = false;
        for (at, row) in next.iter().enumerate() {
            if !drawn[at].matches(row) {
                model.splice(
                    u32::try_from(at).unwrap_or(u32::MAX),
                    1,
                    std::slice::from_ref(row),
                );
                changed = true;
            }
        }
        return changed;
    }

    let shared = drawn.len().min(next.len());
    let before = (0..shared)
        .take_while(|i| drawn[*i].matches(&next[*i]))
        .count();
    let after = (0..shared - before)
        .take_while(|i| drawn[drawn.len() - 1 - i].matches(&next[next.len() - 1 - i]))
        .count();
    model.splice(
        u32::try_from(before).unwrap_or(u32::MAX),
        u32::try_from(drawn.len() - before - after).unwrap_or(u32::MAX),
        &next[before..next.len() - after],
    );
    true
}

fn build_post_list(
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    store: &Rc<RefCell<Store>>,
    server: &Rc<RefCell<String>>,
    viewer: &Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
    shown: &ShownTools,
    links: &LinkImages,
    // `relative`: say how long ago each message was instead of what the
    // clock said. The thread pane has no day separators, so a bare
    // "17:01" there could be today or a fortnight ago.
    relative: bool,
) -> (gio::ListStore, gtk::ListView, gtk::ScrolledWindow) {
    let posts = gio::ListStore::new::<PostRow>();
    let factory = gtk::SignalListItemFactory::new();

    let sender = commands.clone();
    let store_for_setup = Rc::clone(store);
    let server_for_setup = Rc::clone(server);
    let shown_for_setup = Rc::clone(shown);
    factory.connect_setup(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            setup_post_row(
                item,
                &sender,
                &store_for_setup,
                &server_for_setup,
                &shown_for_setup,
            );
        }
    });
    let sender = commands.clone();
    let viewer_for_bind = Rc::clone(viewer);
    let shown_for_bind = Rc::clone(shown);
    let links_for_bind = Rc::clone(links);
    factory.connect_bind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            // This widget may have been showing the previous occupant's
            // toolbar. The message under it has just changed, so the
            // toolbar is wrong whatever the pointer is doing.
            hide_tools(item, &shown_for_bind);
            bind_post_row(item, &sender, &viewer_for_bind, &links_for_bind, relative);
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
                // Kept whole, scheme and all: a link to a message has to be
                // one somebody can paste into a browser.
                self.server.replace(server.clone());
                self.set_status("connected");
            }
            Event::Threading(threading) => {
                self.store.borrow_mut().threading = threading;
            }
            Event::Posts { channel, page } => {
                self.store
                    .borrow_mut()
                    .apply_post_list(&channel, &page, PageAnchor::Newest);
                if self.selected.borrow().as_ref() == Some(&channel)
                    && *self.pane.borrow() == Pane::Channel
                {
                    self.refresh_posts(&channel);
                }
            }
            Event::Ws(mm_api::ws::WsEvent::Typing {
                channel_id,
                user_id,
                ..
            }) => self.on_typing(channel_id, UserId::new(&user_id)),
            Event::Ws(ws) => {
                self.maybe_notify(&ws);
                let changes = self.store.borrow_mut().apply_ws(&ws);
                if changes.is_empty() {
                    return;
                }
                // Coalesced, because events arrive in bursts -- a reconnect
                // replays a queue of them -- and each one would otherwise
                // redraw the open channel, the sidebar and the thread pane.
                // A reply to the open thread arrives as an ordinary posted
                // event, and with collapsed threads it is in no channel
                // timeline, so the thread pane has to be in that redraw.
                self.redraw_soon();
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

    /// Put Threads and Drafts above the categories, the way Mattermost
    /// stacks them.
    ///
    /// Threads only when collapsed threads are on: without them there is
    /// nothing to follow, and Mattermost hides the view for the same
    /// reason. Drafts only when there are some, which is also what it
    /// does -- an always-present "Drafts (0)" is a row that never does
    /// anything.
    fn append_view_entries(&self) {
        let store = self.store.borrow();
        if store.threading == Threading::Collapsed {
            let totals = store.thread_totals;
            self.sidebar.append(&view_entry(
                THREADS_ROW,
                "mail-reply-all-symbolic",
                "Threads",
                totals.unread_mentions,
                totals.unread_threads > 0,
            ));
        }
        let drafts = store.pending_drafts().len();
        if drafts > 0 {
            self.sidebar.append(&view_entry(
                DRAFTS_ROW,
                "document-edit-symbolic",
                "Drafts",
                i64::try_from(drafts).unwrap_or(i64::MAX),
                false,
            ));
        }
    }

    /// The quick switcher: type a few letters, press Return, be there.
    ///
    /// Ctrl+K, which is what Mattermost binds it to. A reader with a
    /// hundred channels does not find one by scrolling the sidebar, and
    /// this is the one piece of searching that needs no endpoint at all
    /// -- every channel they are in is already in the store.
    fn open_switcher(&self) {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Find channel…")
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .css_classes(["navigation-sidebar"])
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let window = adw::Window::builder()
            .title("Find channel")
            .modal(true)
            .default_width(SWITCHER_WIDTH)
            .default_height(SWITCHER_HEIGHT)
            .build();
        if let Some(parent) = self.sidebar.root().and_downcast::<gtk::Window>() {
            window.set_transient_for(Some(&parent));
        }
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 6);
        layout.append(&entry);
        layout.append(&scroll);
        window.set_content(Some(&layout));

        let fill = {
            let ui = self.clone();
            let list = list.clone();
            move |query: &str| {
                while let Some(old) = list.first_child() {
                    list.remove(&old);
                }
                for found in ui.store.borrow().find_channels(query, SWITCHER_RESULTS) {
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    line.append(&ui.channel_mark(&found));
                    line.append(
                        &gtk::Label::builder()
                            .label(&found.display_name)
                            .xalign(0.0)
                            .hexpand(true)
                            .ellipsize(gtk::pango::EllipsizeMode::End)
                            .css_classes(if found.is_unread() {
                                ["heading"]
                            } else {
                                ["dim-label"]
                            })
                            .build(),
                    );
                    let row = gtk::ListBoxRow::builder().child(&line).build();
                    row.set_widget_name(found.channel.as_str());
                    list.append(&row);
                }
                // So Return goes somewhere without arrowing down first.
                if let Some(first) = list.row_at_index(0) {
                    list.select_row(Some(&first));
                }
            }
        };
        fill("");
        {
            let fill = fill.clone();
            entry.connect_search_changed(move |entry| fill(entry.text().as_str()));
        }

        let go = {
            let ui = self.clone();
            let window = window.clone();
            move |row: &gtk::ListBoxRow| {
                let channel = ChannelId::new(row.widget_name().as_str());
                window.close();
                if !channel.as_str().is_empty() {
                    ui.show_channel(&channel);
                }
            }
        };
        {
            let go = go.clone();
            list.connect_row_activated(move |_, row| go(row));
        }

        window.add_controller(switcher_keys(&window, &list, go));

        window.present();
        entry.grab_focus();
    }

    /// Show one of the three views, and draw it.
    ///
    /// One place decides what the middle of the window holds, so the
    /// composer, the stack page and the heading can never disagree about
    /// which view is up.
    fn show_pane(&self, pane: &Pane) {
        *self.pane.borrow_mut() = pane.clone();
        match pane {
            Pane::Channel => {
                self.middle.set_visible_child_name("channel");
                self.composer.set_visible(true);
                if let Some(open) = self.selected.borrow().clone() {
                    self.refresh_heading(&open);
                }
            }
            Pane::Threads => {
                self.middle.set_visible_child_name("threads");
                // Nothing to send to: these views are lists of places.
                self.composer.set_visible(false);
                self.refresh_threads_view();
            }
            Pane::Drafts => {
                self.middle.set_visible_child_name("drafts");
                self.composer.set_visible(false);
                self.refresh_drafts_view();
            }
        }
    }

    /// Go to a channel from somewhere that is not the sidebar -- a draft,
    /// so far. Selecting the row is what opens it: the sidebar's own
    /// handler does the fetching and the mark-read, and going around it
    /// would mean two places that know how to open a channel.
    fn show_channel(&self, channel: &ChannelId) {
        self.show_pane(&Pane::Channel);
        let row = (0..)
            .map_while(|i| self.sidebar.row_at_index(i))
            .find(|row| row.widget_name() == channel.as_str());
        if let Some(row) = row {
            self.sidebar.select_row(Some(&row));
        }
    }

    /// Draw the followed threads.
    fn refresh_threads_view(&self) {
        while let Some(old) = self.threads_list.first_child() {
            self.threads_list.remove(&old);
        }
        let store = self.store.borrow();
        let only_unread = self.unreads_only.get();
        let mut drawn = 0;
        for thread in store.followed_threads() {
            if only_unread && !thread.is_unread() {
                continue;
            }
            let Some(post) = &thread.post else { continue };
            let (author, _) = store.post_author(post);
            let chip = store
                .channels
                .get(&post.channel_id)
                .map(|channel| store.channel_label(channel))
                .unwrap_or_default();
            let replies = if thread.reply_count == 1 {
                "1 reply".to_owned()
            } else {
                format!("{} replies", thread.reply_count)
            };
            // Who is in the conversation, in the order the server lists
            // them, and no more than fit on a line.
            let who: Vec<&UserId> = thread
                .participants
                .iter()
                .map(|u| &u.id)
                .take(FACES)
                .collect();
            self.want_avatars(&store, &who);
            let row = view_row(&ViewRow {
                who: &author,
                chip: &chip,
                ago: &describe_ago(thread.last_reply_at),
                preview: &mm_md::parse(&post.message).to_plain(),
                footer: &replies,
                faces: who
                    .iter()
                    .map(|id| self.avatars.borrow().get(*id).cloned())
                    .collect(),
                face_names: who.iter().map(|id| store.user_label(id)).collect(),
                unread: thread.is_unread(),
            });
            row.set_widget_name(thread.id.as_str());
            self.threads_list.append(&row);
            drawn += 1;
        }
        if drawn == 0 {
            self.threads_list.append(&empty_view(if only_unread {
                "No unread threads"
            } else {
                "No followed threads"
            }));
        }
    }

    /// Ask for the faces this view wants, once each, the way the message
    /// list already does for the people in a channel.
    fn want_avatars(&self, store: &Store, who: &[&UserId]) {
        for user in who {
            if self.avatars.borrow().contains_key(*user)
                || !self.asked.borrow_mut().insert((*user).clone())
            {
                continue;
            }
            self.commands
                .send(Command::FetchAvatar {
                    user: (*user).clone(),
                    version: store
                        .users
                        .get(*user)
                        .map_or(mm_types::Timestamp::ZERO, |u| u.last_picture_update),
                })
                .ok();
        }
    }

    /// Draw the unsent messages.
    fn refresh_drafts_view(&self) {
        while let Some(old) = self.drafts_list.first_child() {
            self.drafts_list.remove(&old);
        }
        let store = self.store.borrow();
        for draft in store.pending_drafts() {
            let name = match store.channels.get(&draft.channel_id) {
                Some(channel) => store.channel_label(channel),
                None => draft.channel_id.to_string(),
            };
            let footer = match (draft.is_reply(), draft.file_ids.len()) {
                (true, 0) => "a reply".to_owned(),
                (true, n) => format!("a reply · {n} attached"),
                (false, 0) => String::new(),
                (false, n) => format!("{n} attached"),
            };
            let row = view_row(&ViewRow {
                who: &name,
                chip: "",
                ago: &describe_ago(draft.update_at),
                preview: &mm_md::parse(&draft.message).to_plain(),
                footer: &footer,
                faces: Vec::new(),
                face_names: Vec::new(),
                unread: false,
            });
            row.set_widget_name(draft.channel_id.as_str());
            self.drafts_list.append(&row);
        }
        if self.drafts_list.first_child().is_none() {
            self.drafts_list.append(&empty_view("No drafts"));
        }
    }

    /// Fold a category shut, or open it again.
    ///
    /// Applied here and sent on, rather than waited for: the server owns
    /// the flag, but a sidebar that folds when the network answers reads
    /// as a click that did not land. The next sync states the server's
    /// copy, which is what makes it the one that counts.
    fn toggle_category(&self, id: &str) {
        let Some(category) = self.store.borrow().category(id).cloned() else {
            return;
        };
        let folded = !category.collapsed;
        if self
            .store
            .borrow_mut()
            .set_category_collapsed(id, folded)
            .is_empty()
        {
            return;
        }
        self.refresh_sidebar();
        self.commands
            .send(Command::SetCategory(mm_types::ChannelCategory {
                collapsed: folded,
                ..category
            }))
            .ok();
    }

    /// One channel's row in the sidebar.
    ///
    /// Split out so `refresh_sidebar` stays about which rows to draw
    /// rather than what one looks like.
    fn channel_row(&self, row: &mm_core::SidebarRow) -> gtk::ListBoxRow {
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
        list_row
    }

    fn refresh_sidebar(&self) {
        let sections = self.store.borrow().sidebar_sections();
        // Nothing to redraw, and redrawing anyway would destroy the row
        // under the reader's cursor.
        if !self.sidebar_stale.replace(false)
            && self.drawn_sidebar.borrow().as_ref() == Some(&sections)
        {
            return;
        }
        self.drawn_sidebar.replace(Some(sections.clone()));
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

        self.append_view_entries();

        let mut to_select = None;
        for section in &sections {
            // The filter hides what is caught up, but never the channel
            // the reader is in -- and a category with nothing left to
            // show is not drawn as a bare heading.
            let showing: Vec<&mm_core::SidebarRow> = section
                .visible_rows(keep.as_ref())
                .into_iter()
                .filter(|row| {
                    !self.sidebar_unread_only.get()
                        || row.is_unread()
                        || keep.as_ref() == Some(&row.channel)
                })
                .collect();
            if showing.is_empty() {
                continue;
            }
            self.sidebar.append(&section_header(section));
            for row in showing {
                let list_row = self.channel_row(row);
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
                // selectable: walk to the first row that is. Threads and
                // Drafts are selectable and come first, and landing on
                // one of those would open a list rather than a
                // conversation -- a window that starts on an empty
                // Drafts view reads as a window that failed to load.
                let first = (0..)
                    .map_while(|i| self.sidebar.row_at_index(i))
                    .filter(gtk::ListBoxRow::is_selectable)
                    .find(|row| !row.widget_name().starts_with("mmrs:"));
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
    fn row_for(
        &self,
        store: &Store,
        post: &mm_types::Post,
        grouped: bool,
        // Worked out once by the caller: it ranks every reaction this
        // reader has ever made, and doing that per row made drawing a
        // channel quadratic in its own history.
        quick: &[String],
    ) -> PostRow {
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
            ago: &describe_ago(post.create_at),
            full_time: &full_time(post.create_at.millis()),
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
            quick_reactions: quick.to_vec(),
            // What the author typed, not what is drawn: "copy text" should
            // paste back as markdown, the way it was written.
            plain_body: post.message.clone(),
            avatar,
            images,
            cards: post
                .message_attachments()
                .iter()
                .map(|a| render_card(a, store))
                .collect(),
            previews: post.link_previews(),
            presence: store.presence(&post.user_id),
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
                file: file.id.clone(),
                texture,
                width,
                height,
            });
        }
        out
    }

    /// Whether the server says this channel has never had a message.
    ///
    /// The channel's own counter, not "we hold nothing": the second is
    /// also true of a channel whose page is still in flight.
    fn known_empty(store: &Store, channel: &ChannelId) -> bool {
        store
            .channels
            .get(channel)
            .is_some_and(|c| c.total_msg_count == 0)
    }

    /// What an empty channel says, the way Mattermost opens one.
    fn channel_intro(store: &Store, channel: &ChannelId) -> String {
        let name = store.channel_heading(channel).name;
        let direct = store.channels.get(channel).is_some_and(|c| c.kind.is_dm());
        if direct {
            format!("This is the start of your conversation with {name}")
        } else {
            format!("This is the start of {name}")
        }
    }

    /// Draw the open thread, or hide the panel when there is none.
    fn refresh_thread(&self) {
        let Some(root) = self.thread_root.borrow().clone() else {
            self.thread_pane.set_visible(false);
            return;
        };
        let store = self.store.borrow();
        // No date separators here: a thread is one conversation and the
        // separators would say the same day over and over. Grouping still
        // applies, so a burst of replies from one person reads as one.
        let posts = store.thread_posts(&root);
        let quick = store.favourite_emoji(QUICK_REACTIONS);
        let mut previous: Option<&mm_types::Post> = None;
        let mut rows: Vec<PostRow> = Vec::with_capacity(posts.len());
        for post in &posts {
            let grouped = previous.is_some_and(|p| {
                p.user_id == post.user_id
                    && !p.is_system()
                    && !post.is_system()
                    && post.create_at.millis() - p.create_at.millis() <= 5 * 60 * 1000
            });
            rows.push(self.row_for(&store, post, grouped, &quick));
            previous = Some(post);
        }
        // Only what changed, for the reasons in `refresh_posts`.
        let changed = splice_changed(&self.thread_posts, &rows);
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
        // Only when a reply actually arrived, and only if the reader was
        // already at the end of the thread. A redraw on its own must not
        // move them: a reaction, an avatar or an image landing used to
        // throw somebody reading half way up a long thread to the bottom
        // of it.
        let n = self.thread_posts.n_items();
        if changed && self.thread_stick.get() && n > 0 {
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
                self.sidebar_soon();
            }
            Event::Members(members) => {
                self.store.borrow_mut().apply_members(members);
                self.sidebar_soon();
            }
            Event::Thread { root, page } => {
                let _ = self.store.borrow_mut().apply_thread(&page);
                // A thread just opened is read from its newest reply,
                // whatever the reader was doing in the last one.
                self.thread_stick.set(true);
                *self.thread_root.borrow_mut() = Some(root.clone());
                self.refresh_thread();
            }
            Event::OlderPosts { channel, page } => self.on_older_posts(&channel, &page),
            Event::Avatar { user, image } => self.on_avatar(user, image),
            Event::File { file, image } => self.on_file(file, image),
            Event::LinkImage { url, image } => {
                let bytes = glib::Bytes::from_owned(image);
                if let Ok(texture) = gdk::Texture::from_bytes(&bytes) {
                    self.link_images.borrow_mut().insert(url, Some(texture));
                    // The card is drawn on bind, so the row has to be
                    // drawn again for the picture to reach it.
                    self.redraw_soon();
                }
            }
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
                self.sidebar_soon();
            }
            Event::ChannelsAre(channels) => {
                let changed = !self
                    .store
                    .borrow_mut()
                    .retain_channels(&channels)
                    .is_empty();
                if changed {
                    self.sidebar_soon();
                }
            }
            Event::Threads(list) => {
                self.store.borrow_mut().apply_threads(&list);
                // The badge is not part of the sections the sidebar
                // compares against, so say outright that it has moved.
                self.sidebar_stale.set(true);
                self.sidebar_soon();
                if *self.pane.borrow() == Pane::Threads {
                    self.refresh_threads_view();
                }
            }
            Event::Drafts(drafts) => {
                self.store.borrow_mut().apply_drafts(&drafts);
                // Likewise: whether there is a Drafts row at all depends
                // on this, and the sections have not changed.
                self.sidebar_stale.set(true);
                self.sidebar_soon();
                if *self.pane.borrow() == Pane::Drafts {
                    self.refresh_drafts_view();
                }
            }
            Event::Preferences(prefs) => {
                self.store.borrow_mut().apply_preferences(&prefs);
            }
            Event::Statuses(statuses) => {
                if !self.store.borrow_mut().apply_statuses(&statuses).is_empty() {
                    self.redraw_soon();
                }
            }
            Event::ChannelRoster { channel, users } => {
                self.store
                    .borrow_mut()
                    .apply_channel_roster(&channel, users);
                self.redraw_soon();
            }
            Event::Users(users) => {
                self.store.borrow_mut().apply_users(users);
                // The roster arrives after the posts that needed it -- the
                // authors of a page are fetched from the page itself -- so
                // every row by a newly-known author is still showing a raw
                // user id until this redraw. Coalesced: a sync answers with
                // the roster of every team in turn, and drawing a channel
                // costs far more than waiting out the rest of them.
                self.redraw_soon();
            }
            // Every other variant has its own arm in `on_event`.
            _ => {}
        }
    }

    /// Somebody is typing in a channel.
    ///
    /// Recorded against the clock rather than in the store, and redrawn
    /// once the entry has had time to expire so the line clears itself
    /// without the reader doing anything.
    fn on_typing(&self, channel: ChannelId, who: UserId) {
        // Never announce the reader to themselves. The server sends the
        // event to everybody in the channel, the sender included.
        if who == self.store.borrow().me {
            return;
        }
        let now = std::time::Instant::now();
        {
            let mut typing = self.typing.borrow_mut();
            let people = typing.entry(channel).or_default();
            people.retain(|(id, at)| *id != who && now.duration_since(*at) < TYPING_LIFETIME);
            people.push((who, now));
        }
        self.refresh_typing();
        let ui = self.clone();
        glib::timeout_add_local_once(
            TYPING_LIFETIME + std::time::Duration::from_millis(100),
            move || {
                ui.refresh_typing();
            },
        );
    }

    /// Redraw the "is typing" line for the open channel, dropping
    /// anybody whose last keystroke has gone stale.
    fn refresh_typing(&self) {
        let Some(channel) = self.selected.borrow().clone() else {
            self.typing_line.set_visible(false);
            return;
        };
        let now = std::time::Instant::now();
        let mut typing = self.typing.borrow_mut();
        let people = typing.entry(channel).or_default();
        people.retain(|(_, at)| now.duration_since(*at) < TYPING_LIFETIME);
        let store = self.store.borrow();
        let names: Vec<String> = people
            .iter()
            .filter_map(|(id, _)| store.users.get(id))
            .map(|user| user.name_as(store.name_format))
            .collect();
        let said = match names.as_slice() {
            [] => String::new(),
            [one] => format!("{one} is typing…"),
            [one, two] => format!("{one} and {two} are typing…"),
            _ => "Several people are typing…".to_owned(),
        };
        self.typing_line.set_visible(!said.is_empty());
        self.typing_line.set_text(&said);
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
        // If this is the image somebody is looking at full size, it is
        // the full-size copy they asked for by opening it.
        if let Some((showing, picture)) = self.viewer.borrow().as_ref()
            && *showing == file
        {
            picture.set_paintable(Some(&texture));
        }
        self.images.borrow_mut().insert(file, texture);
        self.redraw_soon();
    }

    /// Redraw both lists and the sidebar, for a change that affects all of
    /// them -- a setting, rather than one message.
    fn redraw_everything(&self) {
        self.refresh_sidebar();
        // The line belongs to the channel on screen, so switching away
        // from one somebody is typing in has to clear it.
        self.refresh_typing();
        // Whatever is actually on show. Redrawing the conversation while
        // a view is up put the channel's name back in the title bar the
        // moment an avatar landed -- and spent the cost of drawing a
        // channel nobody was looking at.
        match &*self.pane.borrow() {
            Pane::Channel => {
                if let Some(open) = self.selected.borrow().clone() {
                    self.refresh_posts(&open);
                }
            }
            Pane::Threads => self.refresh_threads_view(),
            Pane::Drafts => self.refresh_drafts_view(),
        }
        self.refresh_thread();
    }

    /// Redraw soon, once, however many times this is called first.
    ///
    /// For things that arrive in a stream -- a channel's worth of avatars,
    /// a channel's worth of images -- where each one changes a little of
    /// the view and redrawing per arrival means redrawing the whole channel
    /// per arrival.
    fn sidebar_soon(&self) {
        if self.sidebar_queued.replace(true) {
            return;
        }
        let ui = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            ui.sidebar_queued.set(false);
            ui.refresh_sidebar();
        });
    }

    fn redraw_soon(&self) {
        if self.redraw_queued.replace(true) {
            return;
        }
        let ui = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            ui.redraw_queued.set(false);
            ui.redraw_everything();
        });
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
        // A face is drawn in the sidebar for a DM, and the rows it is
        // built from have not changed, so say so explicitly.
        self.sidebar_stale.set(true);
        self.redraw_soon();
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
            let (gutter, face, dot) = face_with_dot(20);
            face.set_text(Some(&row.display_name));
            face.set_custom_image(self.avatars.borrow().get(user));
            // The sidebar is the one place presence is actually used for
            // a decision: it is where the reader picks who to ask.
            set_presence(&dot, self.store.borrow().presence(user));
            return gutter.upcast();
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
    /// What the window's title bar says. One place, so the two views and
    /// the conversation cannot leave each other's heading up.
    fn set_title(&self, name: &str, under: &str) {
        self.title.set_title(name);
        self.title.set_subtitle(under);
    }

    fn refresh_heading(&self, channel: &ChannelId) {
        let heading = self.store.borrow().channel_heading(channel);
        self.title.set_title(&heading.name);
        // Members and the channel's own header line, in that order, the way
        // Mattermost puts them beside the name. Either may be absent.
        let members = match heading.members {
            // "1 member" would be the reader alone in a channel, which is a
            // real state and reads badly in the plural.
            Some(1) => Some("1 member".to_owned()),
            Some(n) => Some(format!("{n} members")),
            None => None,
        };
        let topic = (!heading.topic.is_empty()).then_some(heading.topic);
        let subtitle = [members, topic]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("  ·  ");
        self.title.set_subtitle(&subtitle);
    }

    fn refresh_posts(&self, channel: &ChannelId) {
        self.refresh_heading(channel);
        // Was the newest message in view before this update? Asked of the
        // model, not of the scroll adjustment: `upper` is an estimate GTK
        // revises as rows are realized, and it was observed moving 20% in both
        // directions inside one run. See docs/gtk-notes.md.
        let was_at_end = self.posts.n_items() == 0 || self.stick_to_bottom.get();

        let store = self.store.borrow();
        // Date separators and author grouping are computed in mm-core, where
        // they are covered: they are calendar arithmetic and identity
        // comparisons, and both are wrong in ways only a test notices.
        let visible = store.channel_posts(channel);
        let offset = local_utc_offset_secs();
        let unread_from = *self.unread_from.borrow();
        let quick = store.favourite_emoji(QUICK_REACTIONS);
        let mut rows: Vec<PostRow> = Vec::with_capacity(visible.len());
        for row in mm_core::rows::project(&visible, offset, unread_from) {
            let (id, grouped) = match row {
                mm_core::Row::Date { at, date } => {
                    rows.push(PostRow::separator(&describe_day(at, &date)));
                    continue;
                }
                mm_core::Row::NewMessages => {
                    rows.push(PostRow::new_messages());
                    continue;
                }
                mm_core::Row::Post { id, grouped } => (id, grouped),
            };
            let Some(post) = store.posts.get(&id) else {
                continue;
            };
            rows.push(self.row_for(&store, post, grouped, &quick));
        }
        // A channel nobody has written in yet says so. Without this it
        // draws as a blank pane, which reads as a channel that failed to
        // load -- there is no way for the reader to tell the difference,
        // and the first report of it was exactly that.
        //
        // Only when the channel is *known* empty, from the message count
        // the server keeps on it, never merely because nothing has
        // arrived yet: saying "no messages" for the third of a second
        // before a page lands would be worse than the blank.
        if rows.is_empty() && Self::known_empty(&store, channel) {
            rows.push(PostRow::separator(&Self::channel_intro(&store, channel)));
        }

        // Only what changed. Appending emits `items_changed` per item and
        // `ListView` answers each one by measuring what it holds, so the
        // rows are built first and handed over together; and because
        // `ListView` realizes everything it is handed, only the run that
        // actually differs is handed over at all. See docs/gtk-notes.md.
        splice_changed(&self.posts, &rows);
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
