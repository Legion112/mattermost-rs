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
//! 3. **No per-widget `glib::spawn_future_local`.** Every per-widget spawn is
//!    a place where a future outlives its widget, so widgets send commands
//!    instead of spawning. [`run`] has two, both owning an `Rc<Ui>` that
//!    outlives the window by construction: the engine's event pump, and a
//!    one-shot await on the spelling dictionaries, which are three and a half
//!    megabytes of Russian and must not be parsed on this thread.
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
///
/// `Debug` is written by hand rather than derived: `remember` is a
/// closure and there is nothing useful to print about one.
pub struct Wiring {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    /// Start in the tray rather than with a window on screen, which is
    /// what `--hidden` and the autostart entry ask for. Honoured only
    /// if a tray actually appears; see [`HIDDEN_GRACE`].
    pub hidden: bool,
    pub events: async_channel::Receiver<Event>,
    /// How big the window was when it was last closed. The window is
    /// where this client lives -- half a screen, usually -- and opening
    /// at a default size every launch means dragging it into place
    /// every launch.
    pub restored: mm_desktop::Restored,
    /// Where to write the size back to. A closure rather than a path,
    /// because which file and which directory is the binary's business
    /// and the window's only job is to say what size it ended up.
    pub remember: Rc<dyn Fn(mm_desktop::Restored)>,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("restored", &self.restored)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
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
    /// Clears every followed thread at once, which is the only thing
    /// to do with a morning's backlog of them.
    read_all_threads: gtk::Button,
    drafts_list: gtk::ListBox,
    sidebar: gtk::ListBox,
    identity: gtk::Label,
    /// This reader's own dot, which is also the button that moves it.
    presence: gtk::Button,
    /// Decoded avatars, by user. A texture is a GPU-side object and must be
    /// built on this thread, so the engine sends bytes and the decode
    /// happens here.
    avatars: Avatars,
    /// Who has typed in a channel lately, and when they last did.
    ///
    /// View state, not store state: it expires on a clock, and the store
    /// is deliberately clock-free so that replaying events is exact. An
    /// entry older than `TYPING_LIFETIME` is simply not shown.
    typing: Typing,
    typing_line: gtk::Label,
    /// The page shared by search, pinned and saved.
    search_entry: gtk::SearchEntry,
    /// Which page of search results is on screen, and whether the
    /// server filled it -- the only evidence that another one exists,
    /// since it states no total.
    search_page: Rc<std::cell::Cell<u32>>,
    search_more: Rc<std::cell::Cell<bool>>,
    found_title: gtk::Label,
    found_list: gtk::ListBox,
    /// Pictures fetched for link previews, shared with the post rows that
    /// draw them.
    link_images: LinkImages,
    /// The server's own emoji: which names it has, and the pictures
    /// fetched so far. Shared with the rows and both composers.
    emoji: Emoji,
    /// The dictionaries, once they have finished loading. `None` until
    /// then, and on a machine with none installed, which is why nothing
    /// waits on it.
    spelling: Rc<RefCell<Option<crate::spelling::Spelling>>>,
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
    /// The channel that was open when the window last closed, until it
    /// has been opened again. Taken rather than read, so that a later
    /// redraw cannot drag the reader back to it after they have moved.
    reopen: Rc<RefCell<Option<ChannelId>>>,
    /// Whether what is selected is only the first-channel fallback,
    /// chosen because nothing better was known yet. A provisional
    /// choice may be overridden when the channel being restored finally
    /// turns up; one the reader made never is.
    provisional: Rc<std::cell::Cell<bool>>,
    /// The mention count the dock was last told about, so it is only
    /// told when the number actually moves rather than on every redraw.
    /// Starts at [`NO_BADGE_YET`].
    badge: Rc<std::cell::Cell<i64>>,
    /// A `mattermost://` link naming a channel this client has not
    /// heard of yet. Retried until the channel list arrives.
    pending_link: Rc<RefCell<Option<String>>>,
    /// Whether an icon made it into the tray. Closing the window hides
    /// it when there is one and quits when there is not: hiding with
    /// nowhere to click would put the application somewhere the reader
    /// cannot get it back from.
    has_tray: Rc<std::cell::Cell<bool>>,
    thread_follow: gtk::ToggleButton,
    /// True while the Following toggle is being set to match a thread
    /// that has just been opened, so its own handler can tell that
    /// apart from the reader clicking it.
    setting_follow: Rc<std::cell::Cell<bool>>,
    /// What the server last answered for a quick-switcher query, and a
    /// way to redraw the open switcher when a later answer lands.
    /// `None` when no switcher is open, which is also what stops a
    /// late answer from reaching a window that has gone.
    elsewhere: Rc<RefCell<Elsewhere>>,
    refill_switcher: Rc<RefCell<Option<Refill>>>,
    /// The window, and where to write down how it was left.
    window: adw::ApplicationWindow,
    remember: Rc<dyn Fn(mm_desktop::Restored)>,
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
    composer: Composer,
    /// The thread panel's own box, so a half-typed reply can be
    /// written down and brought back like a channel's.
    thread_composer: Composer,
    /// The last half-typed `@` the server was asked about, so asking
    /// again for the same letters costs nothing.
    mentions_asked: Rc<RefCell<String>>,
    /// How to draw the open mention popup again, while one is open --
    /// the server's answer arrives after it was drawn from what the
    /// store knew.
    mentions_refill: Rc<RefCell<Option<PickerRefill>>>,
    /// Set while a composer is being filled from a draft, so putting
    /// yesterday's half-typed message back does not announce to the
    /// channel that somebody is writing.
    restoring: Rc<std::cell::Cell<bool>>,
    /// The frame around it, which is what is shown, hidden and greyed.
    composer_pane: gtk::Box,
}

/// Build the window and run the GTK main loop.
#[must_use]
pub fn run(wiring: Wiring) -> glib::ExitCode {
    let app_id = std::env::var(APP_ID_OVERRIDE).unwrap_or_else(|_| APP_ID.to_owned());
    let app = adw::Application::builder()
        .application_id(app_id)
        // Without this a `mattermost://` URL on the command line is an
        // error rather than something to open, and the `.desktop`
        // entry's handler registration would be accepted and then do
        // nothing with the link.
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    let wiring = Rc::new(RefCell::new(Some(wiring)));
    let started: Rc<RefCell<Option<Rc<Ui>>>> = Rc::new(RefCell::new(None));

    {
        let wiring = Rc::clone(&wiring);
        let started = Rc::clone(&started);
        app.connect_activate(move |app| {
            start(app, &wiring, &started);
        });
    }
    {
        let wiring = Rc::clone(&wiring);
        let started = Rc::clone(&started);
        // Launched with a link, or handed one while already running --
        // `gio::Application` routes the second launch to this instance
        // over D-Bus, which is single-instance for free and is why
        // there is no lock file anywhere in this tree.
        app.connect_open(move |app, files, _hint| {
            let Some(ui) = start(app, &wiring, &started) else {
                return;
            };
            for file in files {
                ui.follow_link(file.uri().as_str());
            }
        });
    }

    // The real arguments, so a URL on the command line reaches `open`
    // -- minus `--hidden`, which is this binary's own and which GTK
    // would reject as an unknown option before anything else ran.
    let argv: Vec<String> = std::env::args().filter(|a| a != HIDDEN_FLAG).collect();
    app.run_with_args(&argv)
}

/// Honour `--hidden`, with a way back if there turns out to be no tray.
///
/// Asked for by the autostart entry, so that logging in does not throw
/// a window at somebody who is looking at something else. **But a
/// hidden window with no tray icon is an application nobody can
/// reach**, and whether a tray appears is not known at startup -- it
/// depends on the desktop. So the window is hidden immediately and
/// shown again after a moment if no icon has turned up.
fn stay_hidden(ui: &Rc<Ui>) {
    ui.window.set_visible(false);
    let ui = Rc::clone(ui);
    glib::timeout_add_local_once(HIDDEN_GRACE, move || {
        if !ui.has_tray.get() {
            ui.window.set_visible(true);
            ui.window.present();
            ui.set_status("no tray to hide in");
        }
    });
}

/// Build the window, or hand back the one that is already up.
///
/// Both entry points go through here: a second launch must present the
/// existing window rather than build a second one, and a link handed to
/// a running instance has to reach the window it already has.
fn start(
    app: &adw::Application,
    wiring: &Rc<RefCell<Option<Wiring>>>,
    started: &Rc<RefCell<Option<Rc<Ui>>>>,
) -> Option<Rc<Ui>> {
    if let Some(ui) = started.borrow().clone() {
        if let Some(window) = app.windows().first() {
            window.present();
        }
        return Some(ui);
    }
    let Wiring {
        commands,
        events,
        restored,
        remember,
        hidden,
    } = wiring.borrow_mut().take()?;

    // No `Command::Sync` here. The engine syncs as soon as it has a
    // session, and asking for a second one ran every request twice --
    // two sets of answers, and the window drew the channel again for
    // each of them.
    let ui = Rc::new(build(app, &commands, restored, &remember));
    *started.borrow_mut() = Some(Rc::clone(&ui));
    if hidden {
        stay_hidden(&ui);
    }

    // THE one bridge. Children never spawn; they send commands.
    let pump = Rc::clone(&ui);
    glib::spawn_future_local(async move {
        while let Ok(event) = events.recv().await {
            pump.on_event(event);
        }
        pump.set_status("the engine stopped");
    });

    // The dictionaries, on a worker thread: `ru_RU.dic` is three and a
    // half megabytes, and parsing it here would hold the window's first
    // paint for as long as it took.
    let reading = gio::spawn_blocking(crate::spelling::Spelling::load);
    let dictionaries = Rc::clone(&ui);
    glib::spawn_future_local(async move {
        if let Ok(spelling) = reading.await {
            dictionaries.use_spelling(spelling);
        }
    });

    Some(ui)
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

/// One row for something the client did not already know about: a
/// channel the reader is not in, or a person.
fn elsewhere_row(action: &str, mark: &str, name: &str, detail: &str) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(
        &gtk::Label::builder()
            .label(mark)
            .width_request(16)
            .css_classes(["dim-label"])
            .build(),
    );
    line.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if !detail.is_empty() {
        line.append(
            &gtk::Label::builder()
                .label(detail)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    let row = gtk::ListBoxRow::builder().child(&line).build();
    // What to do when it is chosen travels on the name, the way a
    // channel id does for the rows above.
    row.set_widget_name(action);
    row
}

/// Redraws the open quick switcher with whatever the server has since
/// answered.
type Refill = Rc<dyn Fn()>;

/// What the server found for a quick-switcher query that the client
/// could not answer from what it holds.
#[derive(Default)]
struct Elsewhere {
    /// The query these answer, so an answer to a query the reader has
    /// already typed past can be ignored rather than drawn.
    term: String,
    channels: Vec<mm_types::Channel>,
    people: Vec<mm_types::User>,
}

/// Faces already fetched and decoded, by whose they are.
type Avatars = Rc<RefCell<HashMap<UserId, gdk::Texture>>>;

/// How a `:shortcode:` the standard table does not have becomes a
/// picture.
///
/// Three things travel together because none of them is useful alone:
/// the store knows which names the server has, the cache holds the ones
/// already decoded, and the sender is how the rest are asked for. The
/// size of the first is the reason there is a cache at all -- a row is
/// bound every time it scrolls past, and fetching on every bind would
/// be a request per frame.
#[derive(Clone)]
struct Emoji {
    store: Rc<RefCell<Store>>,
    /// Decoded pictures by the name they are written as. A present key
    /// means it has been asked for; `None` is "asked, not here yet".
    images: Rc<RefCell<HashMap<String, Option<gdk::Texture>>>>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    /// How to draw the open picker again, while one is open.
    ///
    /// A message row is redrawn when a picture lands because the whole
    /// list is; a popover is not in that list, so it would sit there
    /// showing shortcodes for the pictures it has just asked for. One
    /// slot, because there is one picker: it is set when a picker opens
    /// and cleared when it closes.
    refill: Rc<RefCell<Option<PickerRefill>>>,
    /// Whether a redraw of the picker is already waiting. Opening it
    /// asks for every custom emoji at once, and forty answers must not
    /// be forty rebuilds of the grid.
    nudged: Rc<std::cell::Cell<bool>>,
}

impl Emoji {
    fn new(
        store: &Rc<RefCell<Store>>,
        commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    ) -> Self {
        Self {
            store: Rc::clone(store),
            images: Rc::new(RefCell::new(HashMap::new())),
            commands: commands.clone(),
            refill: Rc::new(RefCell::new(None)),
            nudged: Rc::new(std::cell::Cell::new(false)),
        }
    }

    /// Draw the open picker again shortly, if one is open.
    fn nudge_picker(&self) {
        if self.nudged.get() || self.refill.borrow().is_none() {
            return;
        }
        self.nudged.set(true);
        let me = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            me.nudged.set(false);
            let refill = me.refill.borrow().clone();
            if let Some(refill) = refill {
                refill();
            }
        });
    }

    /// The picture for a shortcode, asking for it the first time.
    ///
    /// `None` covers all three of "not a custom emoji", "asked for and
    /// not here yet" and "the fetch failed" -- which draw the same, as
    /// the shortcode, and that is the right answer for each of them.
    fn texture(&self, name: &str) -> Option<gdk::Texture> {
        let id = self.store.borrow().custom_emoji(name).cloned()?;
        if let Some(held) = self.images.borrow().get(name) {
            return held.clone();
        }
        self.images.borrow_mut().insert(name.to_owned(), None);
        self.commands
            .send(Command::FetchEmoji {
                name: name.to_owned(),
                id,
            })
            .ok();
        None
    }

    /// A picture sized to sit on a line of text.
    fn image(texture: &gdk::Texture) -> gtk::Image {
        let image = gtk::Image::from_paintable(Some(texture));
        image.set_pixel_size(EMOJI_PIXELS);
        // A `GtkImage` fills what it is given by default, and inside a
        // `TextView` line that means claiming height the text does not
        // need; centred on the line it sits where a glyph would.
        image.set_valign(gtk::Align::Center);
        image.set_halign(gtk::Align::Center);
        image
    }

    /// Put a decoded picture in, and say whether it is new -- a repeat
    /// must not cost a redraw of every message on screen.
    ///
    /// Scaled on the way in rather than on the way out. A custom emoji
    /// is uploaded at whatever size its author had -- 128 pixels is
    /// common and 512 happens -- and a texture is drawn inline at its
    /// own size, so the scaling has to happen before it becomes one.
    fn received(&self, name: String, image: Vec<u8>) -> bool {
        let bytes = glib::Bytes::from_owned(image);
        let stream = gio::MemoryInputStream::from_bytes(&bytes);
        let scaled = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
            &stream,
            EMOJI_PIXELS,
            EMOJI_PIXELS,
            true,
            gio::Cancellable::NONE,
        );
        let texture = match scaled {
            Ok(pixbuf) => gdk::Texture::for_pixbuf(&pixbuf),
            // A format gdk-pixbuf will not scale is still worth
            // drawing if GDK can decode it at all.
            Err(_) => match gdk::Texture::from_bytes(&bytes) {
                Ok(texture) => texture,
                Err(_) => return false,
            },
        };
        self.images
            .borrow_mut()
            .insert(name, Some(texture))
            .flatten()
            .is_none()
    }
}

/// Drawing the open emoji picker again with whatever is in its search
/// box now.
type PickerRefill = Rc<dyn Fn()>;

/// How big a custom emoji is drawn. Matches the height of a line of
/// body text closely enough that a reaction chip does not grow when one
/// lands in it.
const EMOJI_PIXELS: i32 = 18;

/// Everything a post row's widgets need to reach, as one thing.
///
/// Each field is an `Rc` or a sender the factory clones per row, and
/// passing them one by one had grown the argument lists past what
/// anybody can read -- and past what clippy will accept.
#[derive(Clone)]
struct RowContext {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    store: Rc<RefCell<Store>>,
    /// The server's base URL, for building a permalink.
    server: Rc<RefCell<String>>,
    /// The image currently open in the viewer, if any.
    viewer: Rc<RefCell<Option<(FileId, gtk::Picture)>>>,
    shown: ShownTools,
    links: LinkImages,
    avatars: Avatars,
    emoji: Emoji,
}

impl RowContext {
    /// Everything the rows share, made once. The caches start empty and
    /// fill as faces, pictures and previews arrive.
    fn new(
        commands: &tokio::sync::mpsc::UnboundedSender<Command>,
        store: &Rc<RefCell<Store>>,
        server: &Rc<RefCell<String>>,
        emoji: &Emoji,
    ) -> Self {
        Self {
            commands: commands.clone(),
            store: Rc::clone(store),
            server: Rc::clone(server),
            viewer: Rc::new(RefCell::new(None)),
            shown: Rc::new(RefCell::new(None)),
            links: Rc::new(RefCell::new(HashMap::new())),
            avatars: Rc::new(RefCell::new(HashMap::new())),
            emoji: emoji.clone(),
        }
    }
}

/// Who has typed in each channel lately, and when they last did.
type Typing = Rc<RefCell<HashMap<ChannelId, Vec<(UserId, std::time::Instant)>>>>;

/// The thread panel: a post list, a heading with a close button, and a
/// composer that replies into the open thread.
///
/// Hidden until a thread is opened — an empty panel permanently taking a
/// third of the window would be worse than no panel.
fn build_thread_pane(
    ctx: &RowContext,
) -> (
    gio::ListStore,
    gtk::ListView,
    gtk::Box,
    gtk::Button,
    gtk::ToggleButton,
    Composer,
    gtk::ScrolledWindow,
) {
    let (thread_posts, thread_view, thread_scroller) = build_post_list(ctx, true);
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
    // Following, where Mattermost puts it: a thread you replied to is
    // followed whether you meant it or not, and the Threads view is
    // only useful if the noisy ones can be dropped from it.
    let thread_follow = gtk::ToggleButton::builder()
        .label("Following")
        .css_classes(["flat", "caption"])
        .tooltip_text("Follow this thread, or stop")
        .build();
    thread_header.append(&thread_title);
    thread_header.append(&thread_follow);
    thread_header.append(&thread_close);
    let thread_composer = build_composer("Reply…", &ctx.emoji);
    let thread_composer_pane = thread_composer.pane.clone();
    let thread_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    thread_pane.append(&thread_header);
    thread_pane.append(&thread_scroller);
    thread_pane.append(&thread_composer_pane);
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
        thread_follow,
        thread_composer,
        thread_scroller,
    )
}

/// The channel list, under a line naming the account this window shows.
fn build_sidebar() -> (
    gtk::ListBox,
    gtk::Label,
    gtk::ToggleButton,
    gtk::Button,
    gtk::Box,
) {
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
    // The reader's own dot, and the only way to move it. Beside the
    // account it belongs to, which is where Mattermost puts it too --
    // behind the avatar there, but this window has no avatar up here.
    let presence = gtk::Button::builder()
        .css_classes(["flat"])
        .tooltip_text("Set your status")
        .child(&presence_dot(mm_types::Presence::Offline))
        .build();

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    identity.set_hexpand(true);
    top.append(&identity);
    top.append(&presence);
    top.append(&unread_only);
    top.set_margin_end(6);

    let sidebar_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_pane.set_size_request(260, -1);
    sidebar_pane.append(&top);
    sidebar_pane.append(&sidebar_scroll);
    (sidebar, identity, unread_only, presence, sidebar_pane)
}

/// A standalone presence dot, for the places there is no face to put
/// one on the corner of.
fn presence_dot(presence: mm_types::Presence) -> gtk::Box {
    let dot = gtk::Box::builder()
        .css_classes(["mmrs-dot", "mmrs-dot-alone"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    // Offline is a drawn ring here rather than nothing. On a message
    // row nothing is right -- a column of grey dots beside old messages
    // is noise -- but a button with nothing on it is a button nobody
    // can find.
    dot.add_css_class(presence.as_str());
    dot.set_tooltip_text(Some(&format!("You are {}", presence.label())));
    dot
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
         .mmrs-dot.dnd { background: #d24b4e; }\n\
         .mmrs-dot-alone { min-width: 10px; min-height: 10px; }\n\
         .mmrs-inline, .mmrs-inline text { background: none; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Write the window's size down when it closes.
///
/// Size and maximized only. On X11 the window manager owns placement,
/// GTK4 deliberately offers no way to ask for a spot, and a client that
/// saved coordinates it cannot restore would be lying in its own state
/// file -- so the window comes back the right *shape* and wherever the
/// desktop decides to put it.
fn remember_geometry(ui: &Ui) {
    let on_close = ui.clone();
    ui.window.connect_close_request(move |window| {
        on_close.remember_state();
        // Whatever is half-typed, before the window goes away -- a
        // close to the tray included, since the process may be killed
        // from there without coming back.
        on_close.remember_drafts();
        if !on_close.has_tray.get() {
            return glib::Propagation::Proceed;
        }
        // Closed to the tray, the way every chat client behaves:
        // notifications keep arriving and the unread count keeps
        // moving, which is the entire point of having the icon.
        // Only when there *is* an icon to click -- hiding with
        // nowhere to click would be hiding the application.
        window.set_visible(false);
        glib::Propagation::Stop
    });
}

/// The window itself: its title bar, the sign-in page, and the stack
/// that swaps between that page and the chat view.
fn build_window(
    app: &adw::Application,
    split: &gtk::Paned,
    geometry: mm_desktop::Geometry,
) -> (
    adw::WindowTitle,
    gtk::Button,
    [gtk::Button; 4],
    Rc<LoginPage>,
    gtk::Stack,
    adw::ApplicationWindow,
) {
    let title = adw::WindowTitle::new("mattermost-rs", "connecting…");
    // The name is a button, the way Mattermost makes it one: it is
    // where the channel's own menu comes from, and a title that opens
    // nothing is a place people click and get nothing.
    let channel_menu = gtk::Button::builder()
        .child(&title)
        .css_classes(["flat"])
        .tooltip_text("Channel info and members")
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&channel_menu)
        .build();
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Reload channels"));
    header.pack_end(&refresh);
    let saved = gtk::Button::from_icon_name("user-bookmarks-symbolic");
    saved.set_tooltip_text(Some("Saved messages"));
    header.pack_end(&saved);
    let pinned = gtk::Button::from_icon_name("view-pin-symbolic");
    pinned.set_tooltip_text(Some("Pinned in this channel"));
    header.pack_end(&pinned);
    let search = gtk::Button::from_icon_name("system-search-symbolic");
    search.set_tooltip_text(Some("Search messages  (Ctrl+Shift+F)"));
    header.pack_end(&search);

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
        .default_width(geometry.width)
        .default_height(geometry.height)
        .content(&root)
        .maximized(geometry.maximized)
        .build();
    window.present();

    (
        title,
        refresh,
        [search, pinned, saved, channel_menu],
        login,
        stack,
        window,
    )
}

// Most of this function is one struct literal naming every widget the
// window holds, exactly once. Splitting it to satisfy a line count
// would mean passing those same widgets through a second signature --
// more lines, one more place for a field to go missing, and nothing
// easier to read.
#[allow(clippy::too_many_lines)]
fn build(
    app: &adw::Application,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    restored: mm_desktop::Restored,
    remember: &Rc<dyn Fn(mm_desktop::Restored)>,
) -> Ui {
    install_styles();
    let (sidebar, identity, unread_only, presence, sidebar_pane) = build_sidebar();

    // The store and the server's URL are made here rather than in the
    // `Ui` below, because the post list's widgets are built before it
    // and the copy-link button needs both: a permalink is the server,
    // the team and the post, and only the store knows the last two.
    //
    // `restored` is taken apart at the same time: the size goes to the
    // window, the channel becomes the one to go back to.
    let store = Rc::new(RefCell::new(Store::new(UserId::default(), Threading::Flat)));
    let server = Rc::new(RefCell::new(String::new()));
    let mm_desktop::Restored { geometry, channel } = restored;
    let reopen = (!channel.is_empty()).then(|| ChannelId::new(channel.as_str()));
    let emoji = Emoji::new(&store, commands);
    let rows = RowContext::new(commands, &store, &server, &emoji);
    let (posts, post_view, scroller) = build_post_list(&rows, false);

    let (composer, status) = build_composer_row(&emoji);

    // The conversation, the Threads view and the Drafts view take turns
    // in the same space. The status line and the composer stay put under
    // all three -- the composer insensitive for the two that have nothing
    // to send to.
    let panes = build_middle(&scroller);
    let Middle {
        threads_list,
        drafts_list,
        unreads_tab,
        read_all_threads,
        search_entry,
        found_title,
        found_list,
        stack: middle,
    } = panes;

    let typing_line = build_typing_line();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&middle);
    content.append(&typing_line);
    content.append(&status);
    content.append(&composer.pane);

    let (
        thread_posts,
        thread_view,
        thread_pane,
        thread_close,
        thread_follow,
        thread_composer,
        thread_scroller,
    ) = build_thread_pane(&rows);

    let (conversation, split) = build_panes(&sidebar_pane, &content, &thread_pane);

    let (title, refresh, [search, pinned, saved, channel_menu], login, stack, window) =
        build_window(app, &split, geometry);

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
        read_all_threads: read_all_threads.clone(),
        drafts_list: drafts_list.clone(),
        sidebar: sidebar.clone(),
        identity: identity.clone(),
        presence: presence.clone(),
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
        reopen: Rc::new(RefCell::new(reopen)),
        provisional: Rc::new(std::cell::Cell::new(false)),
        badge: Rc::new(std::cell::Cell::new(NO_BADGE_YET)),
        pending_link: Rc::new(RefCell::new(None)),
        has_tray: Rc::new(std::cell::Cell::new(false)),
        thread_follow: thread_follow.clone(),
        setting_follow: Rc::new(std::cell::Cell::new(false)),
        elsewhere: Rc::new(RefCell::new(Elsewhere::default())),
        refill_switcher: Rc::new(RefCell::new(None)),
        window: window.clone(),
        remember: Rc::clone(remember),
        avatars: Rc::clone(&rows.avatars),
        typing: Rc::new(RefCell::new(HashMap::new())),
        typing_line,
        search_entry: search_entry.clone(),
        search_page: Rc::new(std::cell::Cell::new(0)),
        search_more: Rc::new(std::cell::Cell::new(false)),
        found_title,
        found_list: found_list.clone(),
        link_images: Rc::clone(&rows.links),
        emoji: rows.emoji.clone(),
        spelling: Rc::new(RefCell::new(None)),
        images: Rc::new(RefCell::new(HashMap::new())),
        viewer: Rc::clone(&rows.viewer),
        asked: Rc::new(RefCell::new(HashSet::new())),
        asked_files: Rc::new(RefCell::new(HashSet::new())),
        posts,
        post_view,
        scroller,
        stick_to_bottom: Rc::new(std::cell::Cell::new(true)),
        status,
        title,
        composer: composer.clone(),
        thread_composer: thread_composer.clone(),
        mentions_asked: Rc::new(RefCell::new(String::new())),
        mentions_refill: Rc::new(RefCell::new(None)),
        restoring: Rc::new(std::cell::Cell::new(false)),
        composer_pane: composer.pane.clone(),
        stack,
        login: Rc::clone(&login),
    };

    // Before `wire`: both install a capture-phase Return handler on the
    // composer, and the one that accepts a half-typed name has to see it
    // first or every completion would send the message instead.
    wire_found(&ui, &search, &pinned, &saved);
    {
        let ui = ui.clone();
        channel_menu.connect_clicked(move |button| {
            ui.open_channel_menu(button.upcast_ref::<gtk::Widget>());
        });
    }
    wire_everything(&ui, &composer, &thread_composer);
    wire(&ui, &refresh, &sidebar, &composer, commands);
    wire_unread_filter(&ui, &unread_only);
    wire_thread(&ui, &thread_close, &thread_composer, commands);
    wire_follow(&ui, &thread_follow);
    wire_login(&login, commands);
    ui
}

/// The handlers that need nothing but the finished `Ui`.
fn wire_everything(ui: &Ui, composer: &Composer, thread_composer: &Composer) {
    attach_both_composers(ui, &composer.view, &thread_composer.view);
    wire_views(ui);
    wire_switcher(ui, &ui.window);
    wire_presence(ui);
    watch_presence(ui);
    remember_geometry(ui);
}

/// Ask again, now and then, where the people on screen are.
///
/// Presence arrives with a sync and is then kept current by
/// `status_change` -- but somebody who walks away produces no event,
/// so after an hour at the desk the sidebar is a wall of green dots
/// about people who went home. Mattermost's own clients re-poll; this
/// does it for the faces actually drawn, which is the sidebar's DMs
/// and the open channel's authors, and never for the whole server.
fn watch_presence(ui: &Ui) {
    let ui = ui.clone();
    glib::timeout_add_local(PRESENCE_REPOLL, move || {
        let who = ui.faces_on_screen();
        if !who.is_empty() {
            ui.commands.send(Command::Statuses(who)).ok();
        }
        glib::ControlFlow::Continue
    });
}

/// The dot beside the account name, and the menu that moves it.
fn wire_presence(ui: &Ui) {
    let menu = ui.clone();
    ui.presence.connect_clicked(move |button| {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let popup = gtk::Popover::builder().child(&list).build();
        popup.set_parent(button);
        popup.connect_closed(gtk::Popover::unparent);

        let now = menu.my_presence();
        for presence in mm_types::Presence::ALL {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_start(6);
            line.set_margin_end(6);
            line.append(&presence_dot(*presence));
            let label = gtk::Label::new(Some(presence.label()));
            // Capitalised the way a menu entry is, without a second
            // spelling of each state to keep in step with the first.
            label.set_markup(&format!(
                "<span>{}{}</span>",
                presence.label()[..1].to_uppercase(),
                glib::markup_escape_text(&presence.label()[1..])
            ));
            line.append(&label);
            let row = gtk::ListBoxRow::builder().child(&line).build();
            // The one it is already on is shown but not offered: asking
            // the server to set what it already says is a round trip
            // for nothing.
            row.set_activatable(*presence != now);
            row.set_sensitive(*presence != now);
            row.set_widget_name(presence.as_str());
            list.append(&row);
        }

        let ui = menu.clone();
        let popdown = popup.clone();
        list.connect_row_activated(move |_, row| {
            ui.commands
                .send(Command::SetPresence(mm_types::Presence::parse(
                    &row.widget_name(),
                )))
                .ok();
            popdown.popdown();
        });
        popup.popup();
    });
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
    composer: &Composer,
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
    let into = composer.clone();
    let ui = ui.clone();
    composer.view.add_controller(submit_keys(move || {
        let text = composer_text(&into.view);
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
                attach: into.take_attachments(),
            })
            .ok();
        into.view.buffer().set_text("");
        ui.remember_drafts();
    }));
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
fn build_middle(scroller: &gtk::ScrolledWindow) -> Middle {
    let (threads_list, threads_scroll) = build_view_list();
    let (unreads_tab, read_all_threads, threads_page) = build_threads_page(&threads_scroll);
    let (drafts_list, drafts_page) = build_view_list();
    let (search_entry, found_title, found_list, found_page) = build_found_page();
    let stack = gtk::Stack::new();
    stack.add_named(scroller, Some("channel"));
    stack.add_named(&threads_page, Some("threads"));
    stack.add_named(&drafts_page, Some("drafts"));
    stack.add_named(&found_page, Some("found"));
    stack.set_vexpand(true);
    Middle {
        threads_list,
        drafts_list,
        unreads_tab,
        read_all_threads,
        search_entry,
        found_title,
        found_list,
        stack,
    }
}

/// The pages that take turns in the middle of the window, and the
/// widgets inside them the `Ui` has to reach.
struct Middle {
    threads_list: gtk::ListBox,
    drafts_list: gtk::ListBox,
    unreads_tab: gtk::ToggleButton,
    read_all_threads: gtk::Button,
    search_entry: gtk::SearchEntry,
    found_title: gtk::Label,
    found_list: gtk::ListBox,
    stack: gtk::Stack,
}

/// The Threads view's two tabs over its list, the way Mattermost puts
/// them: everything followed, or only what has new replies.
fn build_threads_page(list: &gtk::ScrolledWindow) -> (gtk::ToggleButton, gtk::Button, gtk::Box) {
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

    // At the far end, the way Mattermost puts it: the one action that
    // applies to the whole list rather than to a row.
    let read_all = gtk::Button::builder()
        .label("Mark all read")
        .css_classes(["flat"])
        .halign(gtk::Align::End)
        .hexpand(true)
        .build();

    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    tabs.set_margin_start(8);
    tabs.set_margin_end(8);
    tabs.set_margin_top(6);
    tabs.set_margin_bottom(2);
    tabs.append(&followed);
    tabs.append(&unreads);
    tabs.append(&read_all);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&tabs);
    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(list);
    // Only the Unreads button is returned of the two tabs: grouping
    // them makes a radio pair, so "followed" is simply "unreads off".
    (unreads, read_all, page)
}

/// A list for one of the two views: the Threads list and the Drafts
/// list are the same furniture with different rows in them.
/// What the line above the results says.
fn found_heading(store: &Store, which: &mm_core::Found, count: usize) -> String {
    match which {
        mm_core::Found::Search if store.search_terms.is_empty() => "Search".to_owned(),
        mm_core::Found::Search => {
            let plural = if count == 1 { "result" } else { "results" };
            format!("{count} {plural} for {}", store.search_terms)
        }
        mm_core::Found::Saved => "Saved messages".to_owned(),
        mm_core::Found::Pinned(channel) => {
            let name = store
                .channels
                .get(channel)
                .map_or_else(String::new, |c| store.channel_label(c));
            format!("Pinned in {name}")
        }
    }
}

/// The page the three answer lists share: a box to type a query into,
/// a line saying what is on show, and the results.
///
/// One page rather than three, because search, pinned and saved differ
/// only in where the list came from and what the heading says. The
/// query box is hidden for the two that are not asked a question.
fn build_found_page() -> (gtk::SearchEntry, gtk::Label, gtk::ListBox, gtk::Box) {
    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search messages…")
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .build();
    let title = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_top(8)
        .margin_bottom(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption-heading"])
        .build();
    let (list, scroll) = build_view_list();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&entry);
    page.append(&title);
    page.append(&scroll);
    (entry, title, list, page)
}

/// Connect the three header buttons and the query box.
fn wire_found(ui: &Ui, search: &gtk::Button, pinned: &gtk::Button, saved: &gtk::Button) {
    {
        let ui = ui.clone();
        search.connect_clicked(move |_| ui.open_search());
    }
    {
        let ui = ui.clone();
        pinned.connect_clicked(move |_| {
            let Some(channel) = ui.selected.borrow().clone() else {
                ui.set_status("open a channel first");
                return;
            };
            ui.commands.send(Command::Pinned(channel.clone())).ok();
            ui.show_pane(&Pane::Found(mm_core::Found::Pinned(channel)));
        });
    }
    {
        let ui = ui.clone();
        saved.connect_clicked(move |_| {
            ui.commands.send(Command::Saved).ok();
            ui.show_pane(&Pane::Found(mm_core::Found::Saved));
        });
    }
    {
        // The server parses the query, so the client waits for Return
        // rather than searching per keystroke: every letter of a
        // half-typed `from:legion` would otherwise be its own search.
        let ui = ui.clone();
        ui.search_entry.clone().connect_activate(move |entry| {
            let terms = entry.text().to_string();
            if terms.trim().is_empty() {
                return;
            }
            let Some(team) = ui.store.borrow().teams.first().map(|t| t.id.clone()) else {
                ui.set_status("no team to search yet");
                return;
            };
            ui.set_status("searching…");
            // A new query starts at the first page, whatever the last
            // one had got to.
            ui.search_page.set(0);
            ui.commands
                .send(Command::Search {
                    team,
                    terms,
                    page: 0,
                })
                .ok();
        });
    }
    {
        let ui = ui.clone();
        ui.found_list.clone().connect_row_activated(move |_, row| {
            let name = row.widget_name();
            if !name.is_empty() {
                ui.go_to_post(&PostId::new(name.as_str()));
            }
        });
    }
}

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
    /// Words to pick out in the preview, for a list of search hits.
    /// Empty everywhere else, and the preview is then plain text.
    highlight: Vec<String>,
    /// A button at the right-hand end of the top line, for a list whose
    /// rows can be acted on without being opened. The caller builds it,
    /// because what it does is the caller's business.
    action: Option<gtk::Button>,
}

/// A message as one run of text, for a list that previews it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
    if let Some(action) = &row.action {
        top.append(action);
    }

    // Two lines of the message and no more: the view is a list of things
    // to go to, not a place to read them.
    //
    // The line breaks come out first. `lines(2)` clamps *wrapped*
    // lines, not the ones the author typed, so a deploy report full of
    // newlines filled the whole page with one result.
    let body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let preview = one_line(row.preview);
    if row.highlight.is_empty() {
        body.set_text(&preview);
    } else {
        body.set_markup(&mm_md::highlight(&preview, &row.highlight));
    }

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
        let ui_all = ui.clone();
        ui.read_all_threads.clone().connect_clicked(move |_| {
            let Some(team) = ui_all.store.borrow().teams.first().map(|t| t.id.clone()) else {
                return;
            };
            if !ui_all
                .store
                .borrow_mut()
                .read_all_threads_locally()
                .is_empty()
            {
                ui_all.sidebar_stale.set(true);
                ui_all.refresh_threads_view();
                ui_all.sidebar_soon();
            }
            ui_all.commands.send(Command::ReadAllThreads(team)).ok();
        });
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

/// The run the caret is sitting at the end of, if it is one.
///
/// Only something being typed *now*: the run has to reach the caret,
/// start at a word boundary -- an address has an `@` in the middle and
/// is not a mention, and `http://` has a colon in it -- and carry no
/// space, since neither a username nor a shortcode has one.
///
/// Returns where the sigil is, counted in characters, and what has been
/// typed after it.
fn being_typed(text: &str, caret: usize, sigil: char) -> Option<(usize, String)> {
    let upto: String = text.chars().take(caret).collect();
    let at = upto.rfind(sigil)?;
    let before = upto[..at].chars().next_back();
    if before.is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let prefix = &upto[at + sigil.len_utf8()..];
    if prefix.chars().any(char::is_whitespace) {
        return None;
    }
    Some((upto[..at].chars().count(), prefix.to_owned()))
}

/// The `@` the caret is sitting at the end of, if it is.
fn mention_being_typed(text: &str, caret: usize) -> Option<(usize, String)> {
    being_typed(text, caret, '@')
}

/// The `:shortcode` the caret is sitting at the end of, if it is.
///
/// Two characters at least, which is Mattermost's own rule and the one
/// thing that keeps a finished `:tada:` from immediately offering the
/// whole table again -- the caret after that last colon is the end of a
/// run with nothing in it. Only shortcode characters, so a smiley typed
/// as `:-)` is left alone.
fn shortcode_being_typed(text: &str, caret: usize) -> Option<(usize, String)> {
    const SHORTEST: usize = 2;
    let (at, prefix) = being_typed(text, caret, ':')?;
    if prefix.chars().count() < SHORTEST
        || !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
    {
        return None;
    }
    Some((at, prefix))
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
///
/// `shown` is whether the face it belongs to is drawn at all. The dot
/// goes with the face and never without it: a grouped message hides the
/// face, and hiding one without the other left a green dot floating on
/// its own in the gutter beside the text.
fn set_presence(dot: &gtk::Box, presence: mm_types::Presence, shown: bool) {
    for state in mm_types::Presence::ALL {
        dot.remove_css_class(state.as_str());
    }
    if !shown || presence == mm_types::Presence::Offline {
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
fn build_composer_row(emoji: &Emoji) -> (Composer, gtk::Label) {
    let composer = build_composer("Write a message…", emoji);
    composer.pane.set_sensitive(false);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_bottom(4)
        .css_classes(["dim-label", "caption"])
        .label("starting…")
        .build();
    (composer, status)
}

/// One composer: a `TextView` that grows with what is typed, framed,
/// with a placeholder drawn over it while it is empty.
///
/// A `TextView` rather than an `Entry` because a message is not one
/// line. Shift+Return puts in a newline and Return sends, which is the
/// binding Mattermost uses and the one muscle memory expects -- an
/// `Entry` can express neither, since it has no newline to insert.
fn build_composer(placeholder: &str, emoji: &Emoji) -> Composer {
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        // Tab belongs to the mention popup and then to focus movement,
        // never to the text: nobody indents a chat message.
        .accepts_tab(false)
        .build();
    let hint = gtk::Label::builder()
        .label(placeholder)
        .xalign(0.0)
        .valign(gtk::Align::Start)
        .margin_start(12)
        .margin_top(8)
        // Not a click target, or the placeholder would swallow the click
        // that is trying to put the caret in the box underneath it.
        .can_target(false)
        .css_classes(["dim-label"])
        .build();
    let over = gtk::Overlay::builder().child(&view).build();
    over.add_overlay(&hint);
    view.buffer().connect_changed(move |buffer| {
        hint.set_visible(buffer.char_count() == 0);
    });

    // Grows to a few lines and then scrolls, rather than pushing the
    // conversation off the top of the window.
    let scroll = gtk::ScrolledWindow::builder()
        .child(&over)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(COMPOSER_MAX_HEIGHT)
        .hexpand(true)
        .build();

    let attach = gtk::Button::from_icon_name("mail-attachment-symbolic");
    attach.set_tooltip_text(Some("Attach a file"));
    attach.add_css_class("flat");
    attach.set_valign(gtk::Align::End);
    attach.set_margin_bottom(4);
    attach.set_margin_end(4);

    let emoji_button = gtk::Button::from_icon_name("face-smile-symbolic");
    emoji_button.set_tooltip_text(Some("Insert an emoji"));
    emoji_button.add_css_class("flat");
    emoji_button.set_valign(gtk::Align::End);
    emoji_button.set_margin_bottom(4);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("card");
    row.append(&scroll);
    row.append(&emoji_button);
    row.append(&attach);

    // What has been chosen but not yet sent, above the box it will be
    // sent with. Hidden while empty so an untouched composer is the
    // same height it always was.
    let tray = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tray.set_visible(false);
    tray.set_margin_bottom(4);

    let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane.set_margin_start(12);
    pane.set_margin_end(12);
    pane.set_margin_top(6);
    pane.set_margin_bottom(12);
    pane.append(&tray);
    pane.append(&row);

    let composer = Composer {
        view,
        pane,
        tray,
        chosen: Rc::new(RefCell::new(Vec::new())),
    };
    composer.wire_attach(&attach);
    composer.wire_emoji(&emoji_button, emoji);
    composer.wire_paste();
    composer.wire_drop();
    composer
}

/// One composer: the box, its frame, and what has been attached to it
/// but not yet sent.
#[derive(Clone)]
struct Composer {
    view: gtk::TextView,
    /// The whole column, which is what is shown, hidden and greyed.
    pane: gtk::Box,
    tray: gtk::Box,
    chosen: Rc<RefCell<Vec<crate::bridge::Attachment>>>,
}

/// What to call a pasted image on the server.
///
/// The time, because two screenshots pasted into one message must not
/// arrive under the same name -- the server would take both, and the
/// reader could not tell them apart afterwards.
fn pasted_name() -> String {
    let when = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("pasted-{when}.png")
}

impl Composer {
    /// The smiley beside the paperclip: put an emoji in the message.
    ///
    /// The same picker the hover toolbar uses, and it inserts the
    /// **shortcode** rather than the character. That is what
    /// Mattermost stores and what the server's own clients render, so
    /// a message written here reads the same everywhere -- and it
    /// stays editable as text.
    fn wire_emoji(&self, button: &gtk::Button, emoji: &Emoji) {
        let composer = self.clone();
        // Nothing to keep alive between openings, so the picker owns
        // no state here; `false` is a flag the hover toolbar needs and
        // a composer does not.
        let picking = Rc::new(std::cell::Cell::new(false));
        let emoji = emoji.clone();
        button.connect_clicked(move |button| {
            let into = composer.clone();
            open_emoji_picker(
                button.upcast_ref::<gtk::Widget>(),
                &picking,
                &emoji,
                move |shortcode| into.insert_text(&format!(":{shortcode}: ")),
            );
        });
    }

    /// Put text in at the caret, and leave the caret after it.
    fn insert_text(&self, text: &str) {
        let buffer = self.view.buffer();
        buffer.insert_at_cursor(text);
        self.view.grab_focus();
    }

    /// Ctrl+V with a picture on the clipboard attaches it instead of
    /// pasting its text.
    ///
    /// Whether the clipboard holds an image is asked **synchronously**,
    /// off the advertised formats, because the answer decides then and
    /// there whether to let the key through: reading the image itself
    /// is asynchronous, and by the time it answered the `TextView`
    /// would already have pasted whatever text came with it.
    fn wire_paste(&self) {
        let keys = gtk::EventControllerKey::new();
        let composer = self.clone();
        keys.connect_key_pressed(move |controller, key, _, state| {
            if !matches!(key, gdk::Key::v | gdk::Key::V)
                || !state.contains(gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Proceed;
            }
            let Some(clipboard) = controller.widget().map(|w| w.clipboard()) else {
                return glib::Propagation::Proceed;
            };
            if !clipboard.formats().contain_mime_type("image/png") {
                // Ordinary text: let the box paste it as always.
                return glib::Propagation::Proceed;
            }
            composer.paste_image(&clipboard);
            glib::Propagation::Stop
        });
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        self.view.add_controller(keys);
    }

    /// Files dragged onto the composer are attached to the message.
    fn wire_drop(&self) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let composer = self.clone();
        target.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            composer.attach_dropped(&files);
            true
        });
        self.pane.add_controller(target);
    }

    /// The paperclip: pick files, and show what was picked.
    fn wire_attach(&self, button: &gtk::Button) {
        let composer = self.clone();
        button.connect_clicked(move |button| {
            let window = button.root().and_downcast::<gtk::Window>();
            let picked = composer.clone();
            gtk::FileDialog::builder()
                .title("Attach to this message")
                .modal(true)
                .build()
                .open_multiple(window.as_ref(), gio::Cancellable::NONE, move |chosen| {
                    // A cancelled dialog is an error here, and is
                    // exactly as uninteresting as it sounds.
                    let Ok(files) = chosen else { return };
                    for file in files.iter::<gio::File>().flatten() {
                        if let Some(path) = file.path() {
                            picked
                                .chosen
                                .borrow_mut()
                                .push(crate::bridge::Attachment::File(path));
                        }
                    }
                    picked.refresh_tray();
                });
        });
    }

    /// Draw a chip per attached file, each with a way to take it off.
    fn refresh_tray(&self) {
        while let Some(old) = self.tray.first_child() {
            self.tray.remove(&old);
        }
        let chosen = self.chosen.borrow().clone();
        self.tray.set_visible(!chosen.is_empty());
        for (at, attachment) in chosen.iter().enumerate() {
            let name = attachment.name();
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chip.add_css_class("mmrs-chip");
            chip.append(&gtk::Image::from_icon_name("mail-attachment-symbolic"));
            chip.append(
                &gtk::Label::builder()
                    .label(&name)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .max_width_chars(28)
                    .build(),
            );
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Do not attach this"));
            let composer = self.clone();
            remove.connect_clicked(move |_| {
                {
                    // By position, read at click time: taking the first
                    // of three off renumbers the other two.
                    let mut chosen = composer.chosen.borrow_mut();
                    if at < chosen.len() {
                        chosen.remove(at);
                    }
                }
                composer.refresh_tray();
            });
            chip.append(&remove);
            self.tray.append(&chip);
        }
    }

    /// Take an image off the clipboard and attach it.
    ///
    /// Pasting a screenshot is how most images get into a chat, and
    /// without this every one of them is: save it somewhere, click the
    /// paperclip, find it again, and remember to delete it later.
    ///
    /// Written to the cache directory under a name with the time in
    /// it, because an upload reads from a path and because two
    /// screenshots pasted a minute apart must not be the same file.
    /// Nothing cleans these up yet, which is the honest cost of doing
    /// it this way rather than holding the bytes.
    fn paste_image(&self, clipboard: &gdk::Clipboard) {
        let composer = self.clone();
        clipboard.read_texture_async(gio::Cancellable::NONE, move |found| {
            let Ok(Some(texture)) = found else {
                return;
            };
            composer
                .chosen
                .borrow_mut()
                .push(crate::bridge::Attachment::Pasted {
                    // The time, so two screenshots in one message do
                    // not arrive as one name twice.
                    name: pasted_name(),
                    data: texture.save_to_png_bytes().to_vec(),
                });
            composer.refresh_tray();
        });
    }

    /// Attach files dropped onto the composer.
    fn attach_dropped(&self, files: &gdk::FileList) {
        {
            let mut chosen = self.chosen.borrow_mut();
            chosen.extend(
                files
                    .files()
                    .iter()
                    .filter_map(gio::File::path)
                    .map(crate::bridge::Attachment::File),
            );
        }
        self.refresh_tray();
    }

    /// What to send, and nothing left behind for the next message.
    fn take_attachments(&self) -> Vec<crate::bridge::Attachment> {
        let taken = std::mem::take(&mut *self.chosen.borrow_mut());
        self.refresh_tray();
        taken
    }
}

/// Now, in the milliseconds since the epoch the server counts in.
///
/// The store is deliberately clock-free -- replaying events has to be
/// exact -- so the window is where a timestamp of this client's own
/// comes from.
fn now() -> mm_types::Timestamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    mm_types::Timestamp(i64::try_from(millis).unwrap_or(i64::MAX))
}

/// Everything typed into a composer.
fn composer_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

/// Where the caret is, counted in characters from the start.
fn composer_caret(view: &gtk::TextView) -> usize {
    let buffer = view.buffer();
    let at = buffer.iter_at_mark(&buffer.get_insert()).offset();
    usize::try_from(at).unwrap_or(0)
}

/// Return sends, Shift+Return puts in a newline.
///
/// Capture phase, ahead of the `TextView`'s own handling, which would
/// otherwise insert the newline for both. The mention popup's keys are
/// installed first and stop the event while it is up, so Return there
/// still accepts the name being completed.
fn submit_keys(send: impl Fn() + 'static) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, state| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
            && !state.contains(gdk::ModifierType::SHIFT_MASK)
        {
            send();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys
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

/// Everything the server will tell us about one person, as the card
/// Mattermost opens when their name is clicked.
///
/// Built from what is already held rather than fetched: the roster
/// arrives when a channel opens and carries the name, the job title and
/// the timezone. A field the server withheld -- `email` is stripped for
/// everybody but you unless the admin says otherwise -- is left out
/// rather than drawn empty.
fn profile_card(
    store: &Rc<RefCell<Store>>,
    avatars: &Avatars,
    user: &mm_types::User,
    emoji: &Emoji,
) -> gtk::Popover {
    let commands = &emoji.commands;
    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    column.set_margin_start(18);
    column.set_margin_end(18);
    column.set_margin_top(14);
    column.set_margin_bottom(14);
    column.set_halign(gtk::Align::Center);

    let (gutter, face, dot) = face_with_dot(96);
    face.set_text(Some(user.display_name()));
    face.set_custom_image(avatars.borrow().get(&user.id));
    set_presence(&dot, store.borrow().presence(&user.id), true);
    gutter.set_halign(gtk::Align::Center);
    gutter.set_margin_bottom(10);
    column.append(&gutter);

    let full = user.full_name();
    if !full.is_empty() {
        column.append(
            &gtk::Label::builder()
                .label(&full)
                .css_classes(["title-2"])
                .build(),
        );
    }
    column.append(
        &gtk::Label::builder()
            .label(format!("@{}", user.username))
            .css_classes(["dim-label"])
            .build(),
    );
    if let Some(status) = user.custom_status(utc_now()) {
        column.append(&status_line(&status, emoji));
    }
    if !user.position.is_empty() {
        column.append(
            &gtk::Label::builder()
                .label(&user.position)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    if user.is_bot {
        column.append(
            &gtk::Label::builder()
                .label("BOT")
                .css_classes(["caption-heading", "dim-label"])
                .build(),
        );
    }
    if user.is_deactivated() {
        column.append(
            &gtk::Label::builder()
                .label("This account has been deactivated")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }

    let facts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    facts.set_margin_top(12);
    facts.set_halign(gtk::Align::Start);
    if !user.email.is_empty() {
        facts.append(&profile_fact("Email", &user.email));
    }
    if let Some((zone, now)) = local_time(user) {
        facts.append(&profile_fact(&format!("Local time ({zone})"), &now));
    }
    if facts.first_child().is_some() {
        column.append(&facts);
    }

    let card = gtk::Popover::builder().child(&column).build();
    // The one thing a card is for, after reading it: say something to
    // this person. Not offered for the reader themself -- Mattermost
    // has a self-DM and this is not how anyone reaches it -- nor for
    // somebody who has left, who cannot be written to.
    let me = store.borrow().me.clone();
    if user.id != me && !user.is_deactivated() {
        let message = gtk::Button::builder()
            .label("Message")
            .css_classes(["suggested-action"])
            .margin_top(14)
            .build();
        let commands = commands.clone();
        let who = user.id.clone();
        let card = card.clone();
        message.connect_clicked(move |_| {
            // The server makes the conversation if there is not one
            // yet, and answers with the channel to go to.
            commands.send(Command::OpenDirect(who.clone())).ok();
            card.popdown();
        });
        column.append(&message);
    }
    card
}

/// The smiley that opens the whole emoji table.
///
/// Mattermost puts one next to the reactions it guessed at, for
/// everything the guesses do not cover.
fn any_reaction_button(
    row: &gtk::Box,
    emoji: &Emoji,
    picking: &Rc<std::cell::Cell<bool>>,
) -> gtk::Button {
    let commands = &emoji.commands;
    let store = &emoji.store;
    let any = gtk::Button::from_icon_name("face-smile-symbolic");
    any.add_css_class("flat");
    any.set_tooltip_text(Some("React with any emoji"));
    let commands = commands.clone();
    let store = Rc::clone(store);
    let row = row.clone();
    let picking = Rc::clone(picking);
    let emoji = emoji.clone();
    any.connect_clicked(move |button| {
        // The post is on the row, put there at bind time, because the
        // rows are recycled and this button is made once.
        let post = row.widget_name();
        if post.is_empty() {
            return;
        }
        let post = PostId::new(post.as_str());
        let commands = commands.clone();
        let store = Rc::clone(&store);
        let picking = Rc::clone(&picking);
        picking.set(true);
        open_emoji_picker(
            button.upcast_ref::<gtk::Widget>(),
            &picking,
            &emoji,
            move |shortcode| {
                // A toggle like the quick buttons: picking one already
                // used takes it back.
                let add = !store.borrow().reacted(&post, shortcode);
                commands
                    .send(Command::React {
                        post: post.clone(),
                        emoji: shortcode.to_owned(),
                        add,
                    })
                    .ok();
            },
        );
    });
    any
}

/// A grid of emoji with a search box, anchored under `at`.
///
/// Built fresh per opening rather than kept: it is a few hundred
/// buttons, and keeping one per message row is how a chat client ends
/// up using a gigabyte. `pick` is given the shortcode without colons,
/// which is what both a reaction and a typed `:name:` need.
fn open_emoji_picker(
    at: &gtk::Widget,
    picking: &Rc<std::cell::Cell<bool>>,
    emoji: &Emoji,
    pick: impl Fn(&str) + 'static,
) {
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search emoji…")
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .build();
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(EMOJI_COLUMNS)
        .min_children_per_line(EMOJI_COLUMNS)
        .homogeneous(true)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(EMOJI_HEIGHT)
        .max_content_height(EMOJI_HEIGHT)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_size_request(EMOJI_WIDTH, -1);
    column.append(&search);
    column.append(&scroll);

    let popup = gtk::Popover::builder().child(&column).build();
    popup.set_parent(at);
    let picking = Rc::clone(picking);
    popup.connect_closed(move |popup| {
        // The toolbar is free to go away again now.
        picking.set(false);
        popup.unparent();
    });

    let pick = Rc::new(pick);
    let fill: Rc<dyn Fn(&str)> = {
        let grid = grid.clone();
        let popup = popup.clone();
        let pick = Rc::clone(&pick);
        let emoji = emoji.clone();
        Rc::new(move |query: &str| {
            while let Some(old) = grid.first_child() {
                grid.remove(&old);
            }
            // The server's own first. They are the ones a reader cannot
            // find anywhere else, there are far fewer of them, and on a
            // team that leans on them they are most of what gets picked.
            let custom = emoji
                .store
                .borrow()
                .custom_emoji_matching(query, EMOJI_OFFERED);
            for name in custom {
                grid.append(&picker_button(&emoji, &name, &pick, &popup));
            }
            for found in mm_md::search_emoji(query, EMOJI_OFFERED) {
                grid.append(&picker_button(&emoji, found.shortcode, &pick, &popup));
            }
        })
    };
    fill("");
    // Drawing the grid asked for every custom emoji on it, and the
    // answers arrive after it is already on screen -- so the picker has
    // to be able to draw itself again, with whatever is in the box now.
    *emoji.refill.borrow_mut() = Some({
        let fill = Rc::clone(&fill);
        let search = search.clone();
        Rc::new(move || fill(search.text().as_str()))
    });
    {
        let emoji = emoji.clone();
        popup.connect_closed(move |_| {
            emoji.refill.borrow_mut().take();
        });
    }
    search.connect_search_changed(move |entry| fill(entry.text().as_str()));
    popup.popup();
}

/// One emoji in the picker's grid.
///
/// A custom one draws its picture once the bytes are here and its
/// shortcode until then, which is also what it draws if the fetch never
/// succeeds -- the same fallback the message body has always used.
fn picker_button(
    emoji: &Emoji,
    name: &str,
    pick: &Rc<impl Fn(&str) + 'static>,
    popup: &gtk::Popover,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .css_classes(["flat"])
        .tooltip_text(format!(":{name}:"))
        .build();
    if let Some(texture) = emoji.texture(name) {
        button.set_child(Some(&Emoji::image(&texture)));
    } else {
        button
            .set_label(&mm_md::glyph(name).map_or_else(|| format!(":{name}:"), ToOwned::to_owned));
    }
    let pick = Rc::clone(pick);
    let popup = popup.clone();
    let name = name.to_owned();
    button.connect_clicked(move |_| {
        pick(&name);
        popup.popdown();
    });
    button
}

/// Open the card for whoever the row under this widget is showing.
///
/// Connected once, in setup, because the rows are recycled: the handler
/// asks the list item who it is currently bound to rather than closing
/// over a person.
fn show_profile(
    at: &gtk::Widget,
    item: &gtk::ListItem,
    store: &Rc<RefCell<Store>>,
    avatars: &Avatars,
    emoji: &Emoji,
) {
    let Some(data) = item.item().and_downcast::<PostRow>() else {
        return;
    };
    if data.is_separator() || data.is_system() {
        return;
    }
    let who = store.borrow().users.get(&data.author_id()).cloned();
    let Some(user) = who else {
        // A post whose author has not been fetched yet. Nothing to
        // show, and an empty card would be worse than no card.
        return;
    };
    let card = profile_card(store, avatars, &user, emoji);
    card.set_parent(at);
    // Taken down and detached on close, or every click leaves another
    // popover parented to the same label.
    card.connect_closed(gtk::Popover::unparent);
    card.popup();
}

/// Somebody's custom status: the emoji they chose and the words
/// beside it.
///
/// The emoji may be one of the server's own, so it goes through the
/// same path a message's does -- picture if there is one, glyph if
/// the table knows it, and the shortcode otherwise.
fn status_line(status: &mm_types::CustomStatus, emoji: &Emoji) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.set_halign(gtk::Align::Center);
    line.set_margin_top(4);
    if !status.emoji.is_empty() {
        match emoji.texture(&status.emoji) {
            Some(texture) => line.append(&Emoji::image(&texture)),
            None => line.append(&gtk::Label::new(Some(
                &mm_md::glyph(&status.emoji)
                    .map_or_else(|| format!(":{}:", status.emoji), ToOwned::to_owned),
            ))),
        }
    }
    if !status.text.is_empty() {
        line.append(
            &gtk::Label::builder()
                .label(&status.text)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build(),
        );
    }
    line
}

/// Now, as the comparison a custom status's expiry wants.
fn utc_now() -> mm_types::UtcMoment {
    mm_types::UtcMoment::parse(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// One labelled line on the card.
fn profile_fact(title: &str, value: &str) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Vertical, 0);
    line.set_margin_top(6);
    line.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["dim-label", "caption-heading"])
            .build(),
    );
    line.append(&gtk::Label::builder().label(value).xalign(0.0).build());
    line
}

/// What the clock says where this person is, and the zone's offset.
///
/// `None` when the server never reported a zone, or reported one this
/// build's copy of the timezone database has never heard of -- a
/// missing line is better than a wrong clock.
fn local_time(user: &mm_types::User) -> Option<(String, String)> {
    let zone: chrono_tz::Tz = user.timezone_name()?.parse().ok()?;
    let now = chrono::Utc::now().with_timezone(&zone);
    Some((
        now.format("GMT%:z").to_string(),
        now.format("%H:%M").to_string(),
    ))
}

/// Underline what is not a word, once the typing stops, and offer
/// replacements on a right-click.
fn watch_spelling(ui: &Ui, view: &gtk::TextView) {
    let buffer = view.buffer();
    // Only once per buffer. GTK logs a warning and drops the second
    // one, which is harmless but is noise in a log that should be
    // quiet enough for a real problem to stand out.
    if buffer.tag_table().lookup(MISSPELLED).is_none() {
        buffer.tag_table().add(
            &gtk::TextTag::builder()
                .name(MISSPELLED)
                // Pango's error underline is the squiggle every other text
                // box on the desktop draws, and it is coloured explicitly
                // because the default follows the text and would be white.
                .underline(gtk::pango::Underline::Error)
                .underline_rgba(&gdk::RGBA::new(0.86, 0.30, 0.31, 1.0))
                .build(),
        );
    }

    let pending = Rc::new(std::cell::Cell::new(0u64));
    let ui_for_changes = ui.clone();
    buffer.connect_changed(move |buffer| {
        // Debounced by generation: every keystroke arms a timer, and
        // all but the last one find the count moved on and do nothing.
        let generation = pending.get().wrapping_add(1);
        pending.set(generation);
        let ui = ui_for_changes.clone();
        let pending = Rc::clone(&pending);
        let buffer = buffer.clone();
        glib::timeout_add_local_once(SPELL_DELAY, move || {
            if pending.get() == generation {
                ui.mark_mistakes(&buffer);
            }
        });
    });

    let menu = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    // Capture, so the menu is filled in before the view opens its own.
    menu.set_propagation_phase(gtk::PropagationPhase::Capture);
    let ui = ui.clone();
    let on = view.clone();
    menu.connect_pressed(move |_, _, x, y| {
        offer_corrections(&ui, &on, x, y);
    });
    view.add_controller(menu);
}

/// Put the replacements for the word under the pointer into the view's
/// own context menu, just before it opens.
///
/// `extra-menu` is rebuilt per click rather than kept: the entries are
/// different for every word, and an action is attached to the view so
/// that choosing one knows which span to replace.
fn offer_corrections(ui: &Ui, view: &gtk::TextView, x: f64, y: f64) {
    // Cleared first: last click's suggestions over this click's word
    // would offer corrections for something else entirely.
    view.set_extra_menu(None::<&gio::MenuModel>);
    let Some((word, suggestions)) = correction_for(ui, view, x, y) else {
        return;
    };

    let actions = gio::SimpleActionGroup::new();
    let menu = gio::Menu::new();
    for (at, suggestion) in suggestions.iter().enumerate() {
        let name = format!("fix{at}");
        let action = gio::SimpleAction::new(&name, None);
        let buffer = view.buffer();
        let (start, end) = (word.start, word.end);
        let replacement = suggestion.clone();
        action.connect_activate(move |_, _| {
            let mut from = buffer.iter_at_offset(offset(start));
            let mut to = buffer.iter_at_offset(offset(end));
            buffer.delete(&mut from, &mut to);
            buffer.insert(&mut from, &replacement);
        });
        actions.add_action(&action);
        menu.append(Some(suggestion), Some(&format!("spell.{name}")));
    }
    // Last, under the suggestions, and offered even when there are
    // none: a word with no suggestions is the likeliest one to be a
    // service name somebody wants accepted rather than corrected.
    {
        let learn = gio::SimpleAction::new("learn", None);
        let ui = ui.clone();
        let view = view.clone();
        let word = word.text.clone();
        learn.connect_activate(move |_, _| {
            if let Some(spelling) = ui.spelling.borrow_mut().as_mut() {
                spelling.learn(&word);
            }
            // Redraw at once: an underline that stays after "add to
            // dictionary" reads as a click that missed.
            ui.mark_mistakes(&view.buffer());
        });
        actions.add_action(&learn);
        menu.append(Some("Add to dictionary"), Some("spell.learn"));
    }
    view.insert_action_group("spell", Some(&actions));
    view.set_extra_menu(Some(&menu));
}

/// The misspelled word under the pointer and what to replace it with,
/// or `None` when the pointer is not on one.
fn correction_for(
    ui: &Ui,
    view: &gtk::TextView,
    x: f64,
    y: f64,
) -> Option<(mm_md::spell::Word, Vec<String>)> {
    let spelling = ui.spelling.borrow();
    let spelling = spelling.as_ref()?;
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, pixels(x), pixels(y));
    let at = usize::try_from(view.iter_at_location(bx, by)?.offset()).ok()?;
    let buffer = view.buffer();
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, false).to_string();
    let word = spelling
        .mistakes(&text)
        .into_iter()
        .find(|word| at >= word.start && at < word.end)?;
    let suggestions = spelling.suggestions(&word.text, SUGGESTIONS_OFFERED);
    (!suggestions.is_empty()).then_some((word, suggestions))
}

/// A pointer coordinate as a widget pixel.
///
/// The value came from a click inside this widget, so it is a small
/// positive number; the clamp is there so the conversion needs no
/// assumption rather than because the far end is reachable.
#[allow(clippy::cast_possible_truncation)]
fn pixels(value: f64) -> i32 {
    value.round().clamp(0.0, f64::from(i32::MAX)) as i32
}

/// A character offset as a text-buffer iterator offset.
fn offset(at: usize) -> i32 {
    i32::try_from(at).unwrap_or(i32::MAX)
}

/// Give both composers their `@` popup.
///
/// Each is told which conversation it is in, so the names it offers are
/// that channel's members. The thread's channel comes from its root
/// post, never from whatever is open behind it -- the same mistake that
/// once sent replies to the wrong channel.
fn attach_both_composers(ui: &Ui, composer: &gtk::TextView, thread_composer: &gtk::TextView) {
    for view in [composer, thread_composer] {
        watch_spelling(ui, view);
    }
    for view in [composer, thread_composer] {
        attach_shortcodes(ui, view);
        watch_drafts(ui, view);
        attach_formatting(view);
    }
    watch_typing(ui, composer, |ui| {
        Some((ui.selected.borrow().clone()?, None))
    });
    watch_typing(ui, thread_composer, |ui| {
        let root = ui.thread_root.borrow().clone()?;
        let channel = ui
            .store
            .borrow()
            .posts
            .get(&root)
            .map(|post| post.channel_id.clone())?;
        Some((channel, Some(root)))
    });
    attach_mentions(ui, composer, |ui| ui.selected.borrow().clone());
    attach_mentions(ui, thread_composer, |ui| {
        let root = ui.thread_root.borrow().clone()?;
        let store = ui.store.borrow();
        store.posts.get(&root).map(|post| post.channel_id.clone())
    });
    // Each box edits what was last said where it sends: the channel
    // composer the last message in the channel, the thread's own box
    // the last reply in the thread. Taking the channel's last message
    // from a thread box is the same mistake that once sent replies to
    // the wrong channel.
    attach_edit_last(ui, composer, |ui| {
        let channel = ui.selected.borrow().clone()?;
        let store = ui.store.borrow();
        store.my_last_post(&channel).map(editable)
    });
    // Shift+Up replies to the last message in the channel, whoever
    // wrote it -- Mattermost's own binding, and the fastest way into
    // a thread without reaching for the mouse. Only from the channel
    // box: the thread box is already in one.
    attach_reply_last(ui, composer);
    attach_edit_last(ui, thread_composer, |ui| {
        let root = ui.thread_root.borrow().clone()?;
        let store = ui.store.borrow();
        store.my_last_reply(&root).map(editable)
    });
}

/// Mattermost's formatting keys: bold, italic, strikethrough, code.
///
/// Markers round the selection, and off again when they are already
/// there -- a toggle, the way every editor does it. The arithmetic is
/// in `mm_md::wrap`, where the awkward cases are settled by tests
/// rather than by trying it in the window.
///
/// A toolbar would be the other way to offer this, and is what
/// Mattermost draws; for somebody who types code into chat all day
/// the keys are the thing, and the toolbar is a row of buttons
/// permanently taking height from the conversation.
fn attach_formatting(view: &gtk::TextView) {
    /// Each binding: the key, whether it wants Shift, and the marker.
    const KEYS: &[(gdk::Key, bool, &str)] = &[
        (gdk::Key::b, false, "**"),
        (gdk::Key::i, false, "*"),
        (gdk::Key::X, true, "~~"),
        (gdk::Key::C, true, "`"),
    ];

    let keys = gtk::EventControllerKey::new();
    let box_ = view.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if !state.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let Some((_, _, marker)) = KEYS
            .iter()
            .find(|(wanted, with_shift, _)| *wanted == key && *with_shift == shift)
        else {
            return glib::Propagation::Proceed;
        };
        format_selection(&box_, marker);
        glib::Propagation::Stop
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    view.add_controller(keys);
}

/// Put `marker` round what is selected, or take it off again.
fn format_selection(view: &gtk::TextView, marker: &str) {
    let buffer = view.buffer();
    let (from, to) = buffer.selection_bounds().unwrap_or_else(|| {
        (
            buffer.iter_at_mark(&buffer.get_insert()),
            buffer.iter_at_mark(&buffer.get_insert()),
        )
    });
    let offset = |iter: &gtk::TextIter| usize::try_from(iter.offset()).unwrap_or(0);
    let done = mm_md::wrap(&composer_text(view), offset(&from), offset(&to), marker);

    buffer.set_text(&done.text);
    let place = |at: usize| buffer.iter_at_offset(i32::try_from(at).unwrap_or(i32::MAX));
    buffer.select_range(&place(done.start), &place(done.end));
    view.grab_focus();
}

/// Tell the channel somebody is writing.
///
/// Throttled, because the server forgets a `user_typing` after five
/// seconds whichever client sent it: one frame every four keeps the
/// indicator up for as long as the typing lasts, and sending one per
/// keystroke would be a frame per character for no extra effect.
///
/// Silent while a draft is being put back: filling the box is not
/// somebody writing, and announcing it on every channel switch would
/// tell a whole team you were about to say something when you were
/// only looking.
fn watch_typing(
    ui: &Ui,
    view: &gtk::TextView,
    speaking: impl Fn(&Ui) -> Option<(ChannelId, Option<PostId>)> + 'static,
) {
    let last = Rc::new(std::cell::Cell::new(None::<std::time::Instant>));
    let ui = ui.clone();
    view.buffer().connect_changed(move |buffer| {
        if ui.restoring.get() || buffer.char_count() == 0 {
            return;
        }
        let now = std::time::Instant::now();
        if last
            .get()
            .is_some_and(|then| now.duration_since(then) < TYPING_EVERY)
        {
            return;
        }
        let Some((channel, root)) = speaking(&ui) else {
            return;
        };
        last.set(Some(now));
        ui.commands.send(Command::Typing { channel, root }).ok();
    });
}

/// Write a draft down a few seconds after the typing stops.
///
/// Not on every keystroke: that would be a request per character.
/// Not only on leaving the channel either -- a crash, or a kill from
/// the tray, would take the message with it.
///
/// A generation counter rather than a cancellable timer: a `SourceId`
/// that has already fired cannot be removed, and the bookkeeping to
/// know which is which is longer than this.
fn watch_drafts(ui: &Ui, view: &gtk::TextView) {
    let generation = Rc::new(std::cell::Cell::new(0u64));
    let ui = ui.clone();
    view.buffer().connect_changed(move |_| {
        let mine = generation.get().wrapping_add(1);
        generation.set(mine);
        let ui = ui.clone();
        let generation = Rc::clone(&generation);
        glib::timeout_add_local_once(DRAFT_IDLE, move || {
            if generation.get() == mine {
                ui.remember_drafts();
            }
        });
    });
}

/// Up in an empty composer edits the last thing this reader said.
///
/// Mattermost's own binding, and the one a daily user reaches for
/// without thinking after a typo. **Only when the box is empty**, which
/// is both what the webapp does and what keeps this out of the way of
/// everything else Up means: moving the caret through a message being
/// written, and walking either completion popup, whose runs are never
/// empty.
fn attach_edit_last(
    ui: &Ui,
    view: &gtk::TextView,
    last: impl Fn(&Ui) -> Option<Editable> + 'static,
) {
    let keys = gtk::EventControllerKey::new();
    let ui = ui.clone();
    let box_ = view.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key != gdk::Key::Up || !state.is_empty() || !composer_text(&box_).is_empty() {
            return glib::Propagation::Proceed;
        }
        let Some((post, was)) = last(&ui) else {
            // Nothing of this reader's to edit. Not a complaint: an Up
            // in an empty box is as likely to be a stray key press.
            return glib::Propagation::Proceed;
        };
        open_editor(&box_, &post, &was, &ui.commands);
        glib::Propagation::Stop
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    view.add_controller(keys);
}

/// Shift+Up in an empty composer opens the last message's thread.
///
/// Empty, like the plain Up that edits: a reader partway through a
/// sentence is selecting text with Shift+Up, not reaching for a
/// thread.
fn attach_reply_last(ui: &Ui, view: &gtk::TextView) {
    let keys = gtk::EventControllerKey::new();
    let ui = ui.clone();
    let box_ = view.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key != gdk::Key::Up
            || state != gdk::ModifierType::SHIFT_MASK
            || !composer_text(&box_).is_empty()
        {
            return glib::Propagation::Proceed;
        }
        let last = {
            let channel = ui.selected.borrow().clone();
            let store = ui.store.borrow();
            channel.and_then(|channel| store.last_post(&channel).map(|post| post.id.clone()))
        };
        let Some(post) = last else {
            return glib::Propagation::Proceed;
        };
        ui.commands.send(Command::OpenThread(post)).ok();
        glib::Propagation::Stop
    });
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    view.add_controller(keys);
}

/// A post to edit: which one, and what it says now.
type Editable = (PostId, String);

fn editable(post: &mm_types::Post) -> Editable {
    (post.id.clone(), post.message.clone())
}

/// The list and popover a completion is drawn in, anchored on a
/// composer.
///
/// `autohide(false)` on purpose: an autohiding popover takes the
/// keyboard, and the whole point is that typing carries on underneath
/// it. Above the caret, with no arrow, which is where Mattermost puts
/// both of these.
fn completion_popup(entry: &gtk::TextView) -> (gtk::ListBox, gtk::Popover) {
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
    (list, popup)
}

/// Offer the people a half-typed `@` might mean, and complete it.
///
/// Both composers get one: a mention is as likely in a reply as in a
/// message, and Mattermost offers it in both.
fn attach_mentions(
    ui: &Ui,
    entry: &gtk::TextView,
    channel: impl Fn(&Ui) -> Option<ChannelId> + 'static,
) {
    let (list, popup) = completion_popup(entry);

    {
        let ui = ui.clone();
        let list = list.clone();
        let popup = popup.clone();
        let view = entry.clone();
        entry.buffer().connect_changed(move |_| {
            let caret = composer_caret(&view);
            let Some((_, prefix)) = mention_being_typed(&composer_text(&view), caret) else {
                popup.popdown();
                ui.mentions_refill.borrow_mut().take();
                return;
            };
            let draw = {
                let ui = ui.clone();
                let list = list.clone();
                let popup = popup.clone();
                let prefix = prefix.clone();
                let channel = channel(&ui);
                move || {
                    fill_mentions(&ui, &list, &prefix, channel.as_ref());
                    if list.row_at_index(0).is_none() {
                        popup.popdown();
                    } else {
                        popup.popup();
                    }
                }
            };
            draw();
            // The server's answer lands after this, and the popup is
            // not in any list the redraw walks, so it keeps a way to
            // draw itself again.
            *ui.mentions_refill.borrow_mut() = Some(Rc::new(draw));
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
    entry.add_controller(completion_keys(entry, &list, &popup, accept_mention));
}

/// Offer the emoji a half-typed `:` might mean, and complete it.
///
/// The server's own first, as pictures, and then the standard table --
/// the same order the picker uses, and for the same reason: the custom
/// ones are the ones a reader cannot find anywhere else.
fn attach_shortcodes(ui: &Ui, entry: &gtk::TextView) {
    let (list, popup) = completion_popup(entry);

    {
        let ui = ui.clone();
        let list = list.clone();
        let popup = popup.clone();
        let view = entry.clone();
        entry.buffer().connect_changed(move |_| {
            let caret = composer_caret(&view);
            let Some((_, prefix)) = shortcode_being_typed(&composer_text(&view), caret) else {
                popup.popdown();
                return;
            };
            fill_shortcodes(&ui, &list, &prefix);
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
            accept_shortcode(&entry, &row.widget_name());
            popup.popdown();
        });
    }
    entry.add_controller(completion_keys(entry, &list, &popup, accept_shortcode));
}

/// Draw the emoji that match, picture or glyph on the left and the
/// shortcode beside it -- the shortcode is what gets typed, so it is
/// the part that has to be readable.
fn fill_shortcodes(ui: &Ui, list: &gtk::ListBox, prefix: &str) {
    while let Some(old) = list.first_child() {
        list.remove(&old);
    }
    let custom = ui
        .store
        .borrow()
        .custom_emoji_matching(prefix, SHORTCODES_OFFERED);
    let standard = mm_md::search_emoji(prefix, SHORTCODES_OFFERED);
    for name in &custom {
        list.append(&shortcode_row(&ui.emoji, name));
    }
    for found in standard
        .iter()
        .take(SHORTCODES_OFFERED.saturating_sub(custom.len()))
    {
        list.append(&shortcode_row(&ui.emoji, found.shortcode));
    }
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
}

/// One offered emoji. The name travels on the row's widget name, the
/// way the mention rows carry a username.
fn shortcode_row(emoji: &Emoji, name: &str) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_margin_start(8);
    line.set_margin_end(8);
    line.set_margin_top(4);
    line.set_margin_bottom(4);
    if let Some(texture) = emoji.texture(name) {
        line.append(&Emoji::image(&texture));
    } else {
        let face = gtk::Label::new(Some(mm_md::glyph(name).unwrap_or(" ")));
        face.set_width_chars(2);
        line.append(&face);
    }
    line.append(&gtk::Label::new(Some(&format!(":{name}:"))));

    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.set_widget_name(name);
    row
}

/// Put the chosen shortcode where the half-typed one was, closed with
/// its colon and followed by a space.
fn accept_shortcode(view: &gtk::TextView, name: &str) {
    let caret = composer_caret(view);
    let text = composer_text(view);
    let Some((at, prefix)) = shortcode_being_typed(&text, caret) else {
        return;
    };
    let head: String = text.chars().take(at).collect();
    let tail: String = text.chars().skip(at + 1 + prefix.chars().count()).collect();
    let buffer = view.buffer();
    buffer.set_text(&format!("{head}:{name}: {tail}"));
    // Past the closing colon and the space, which is where somebody
    // mid-sentence wants to carry on.
    let put = at + name.chars().count() + 3;
    let caret = buffer.iter_at_offset(i32::try_from(put).unwrap_or(i32::MAX));
    buffer.place_cursor(&caret);
}

/// How many names the local store has to offer before the server is
/// asked for more. Below this the typing has probably reached somebody
/// this client has never seen.
const MENTIONS_ENOUGH: usize = 3;

/// The shortest half-typed `@` worth a request. One letter matches a
/// third of a company.
const MENTIONS_SHORTEST: usize = 2;

/// The popup's keys, while it is up: move, accept, dismiss.
///
/// Capture phase, like the switcher's: a `GtkEntry` with the focus
/// takes Up, Down, Tab and Return for itself, and the caret is in one.
/// Everything is passed through when the popup is down, so typing is
/// untouched.
fn completion_keys(
    entry: &gtk::TextView,
    list: &gtk::ListBox,
    popup: &gtk::Popover,
    accept: impl Fn(&gtk::TextView, &str) + 'static,
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
                    accept(&entry, &row.widget_name());
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
    // The three that mean a group rather than a person, under their own
    // heading the way Mattermost draws them: they have no avatar, no
    // presence and no id, so they are not faked into the roster.
    let wanted = prefix.trim().to_lowercase();
    let special: Vec<&(&str, &str)> = mm_core::SPECIAL_MENTIONS
        .iter()
        .filter(|(word, _)| word.starts_with(&wanted))
        .collect();
    if !special.is_empty() {
        list.append(&mention_heading("SPECIAL MENTIONS"));
        for (word, what) in special {
            list.append(&special_mention_row(word, what));
        }
    }

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
    drop(store);
    ui.ask_for_mentions(prefix, channel);
}

/// One of `@all`, `@channel`, `@here`, with what it does beside it.
fn special_mention_row(word: &str, what: &str) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_margin_start(12);
    line.set_margin_end(12);
    line.set_margin_top(2);
    line.set_margin_bottom(2);
    // An icon where a face would be, so the rows line up with the
    // people below them.
    let mark = gtk::Image::from_icon_name("system-users-symbolic");
    mark.set_pixel_size(20);
    mark.add_css_class("dim-label");
    line.append(&mark);
    line.append(
        &gtk::Label::builder()
            .label(format!("@{word}"))
            .css_classes(["heading"])
            .build(),
    );
    line.append(
        &gtk::Label::builder()
            .label(what)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build(),
    );
    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.set_widget_name(word);
    row
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
fn accept_mention(view: &gtk::TextView, username: &str) {
    let caret = composer_caret(view);
    let text = composer_text(view);
    let Some((at, prefix)) = mention_being_typed(&text, caret) else {
        return;
    };
    let head: String = text.chars().take(at).collect();
    let tail: String = text.chars().skip(at + 1 + prefix.chars().count()).collect();
    let buffer = view.buffer();
    buffer.set_text(&format!("{head}@{username} {tail}"));
    let put = at + username.chars().count() + 2;
    let caret = buffer.iter_at_offset(i32::try_from(put).unwrap_or(i32::MAX));
    buffer.place_cursor(&caret);
}

/// Ctrl+K, the way Mattermost binds the quick switcher.
///
/// On the window rather than on a widget, so it works wherever the
/// focus is -- including inside the composer, which is where a reader's
/// caret usually is when they decide to go somewhere else.
fn wire_switcher(ui: &Ui, window: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    let ui = ui.clone();
    let parent = window.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key == gdk::Key::k && state.contains(gdk::ModifierType::CONTROL_MASK) {
            ui.open_switcher();
            return glib::Propagation::Stop;
        }
        // Ctrl+Shift+F, the way Mattermost binds search. The shift is
        // what keeps it off Ctrl+F, which GTK gives to find-in-widget.
        if key == gdk::Key::F
            && state.contains(gdk::ModifierType::CONTROL_MASK)
            && state.contains(gdk::ModifierType::SHIFT_MASK)
        {
            ui.open_search();
            return glib::Propagation::Stop;
        }
        // Alt+Up and Alt+Down walk the sidebar, and adding Shift walks
        // only what is unread -- the bindings Mattermost uses, and the
        // fastest way through a morning's backlog without reaching for
        // the mouse once.
        if state.contains(gdk::ModifierType::ALT_MASK) {
            let unread_only = state.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::Up | gdk::Key::KP_Up => {
                    ui.step_channel(false, unread_only);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Down | gdk::Key::KP_Down => {
                    ui.step_channel(true, unread_only);
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }
        // Ctrl+/ shows what all of this is, which is the only way a
        // reader finds out that any of it exists.
        if key == gdk::Key::slash && state.contains(gdk::ModifierType::CONTROL_MASK) {
            open_shortcuts(&parent);
            return glib::Propagation::Stop;
        }
        // Escape puts the thread panel away, which is the only thing on
        // screen that stays open until it is told not to.
        if key == gdk::Key::Escape && ui.thread_pane.is_visible() {
            ui.close_thread();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    // Capture, not bubble: a `GtkEntry` with the focus consumes key
    // presses, and the composer has the focus most of the time.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    window.add_controller(keys);
}

/// Every binding, in the groups a reader would look for them in.
///
/// One table, and the only place any of them is written down for a
/// person to read. It is hand-kept rather than derived: the handlers
/// are spread over a window controller, two composers and two
/// completion popups, and a table that described half of them
/// accurately would be worse than one plainly maintained by hand.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Getting around",
        &[
            ("<Control>k", "Jump to a channel or a person"),
            ("<Control><Shift>f", "Search this team's messages"),
            ("<Alt>Up", "Previous channel"),
            ("<Alt>Down", "Next channel"),
            ("<Alt><Shift>Up", "Previous unread channel"),
            ("<Alt><Shift>Down", "Next unread channel"),
            ("Escape", "Close the thread panel"),
            ("<Control>slash", "Show these shortcuts"),
        ],
    ),
    (
        "Writing",
        &[
            ("Return", "Send"),
            ("<Shift>Return", "New line"),
            ("Up", "Edit your last message, in an empty box"),
            ("<Shift>Up", "Reply to the last message"),
            ("at", "Offer the people in this channel"),
            ("colon", "Offer emoji, the server's own first"),
            ("Tab", "Accept what is offered"),
            ("<Control>b", "Bold"),
            ("<Control>i", "Italic"),
            ("<Control><Shift>x", "Strikethrough"),
            ("<Control><Shift>c", "Code"),
        ],
    ),
];

/// The shortcuts window, built fresh each time it is asked for.
///
/// Fresh rather than kept: it is opened rarely and holding one window
/// per session to save building it once is the wrong trade.
fn open_shortcuts(parent: &adw::ApplicationWindow) {
    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .visible(true)
        .build();
    for (title, bindings) in SHORTCUTS {
        let group = gtk::ShortcutsGroup::builder().title(*title).build();
        for (accelerator, what) in *bindings {
            group.append(
                &gtk::ShortcutsShortcut::builder()
                    .accelerator(*accelerator)
                    .title(*what)
                    .build(),
            );
        }
        section.append(&group);
    }

    let window = gtk::ShortcutsWindow::builder()
        .modal(true)
        .transient_for(parent)
        .build();
    window.set_child(Some(&section));
    window.present();
}

/// Connect the Following toggle on the thread panel.
///
/// Guarded against its own redraw: `set_active` when the panel opens a
/// thread fires `toggled` as surely as a click does, and without the
/// guard opening a followed thread would immediately unfollow it.
fn wire_follow(ui: &Ui, follow: &gtk::ToggleButton) {
    let ui = ui.clone();
    follow.connect_toggled(move |button| {
        if ui.setting_follow.get() {
            return;
        }
        let Some(root) = ui.thread_root.borrow().clone() else {
            return;
        };
        let Some(team) = ui.store.borrow().teams.first().map(|t| t.id.clone()) else {
            return;
        };
        let following = button.is_active();
        if !following {
            // Immediate, because unfollowing is done to make a noisy
            // thread go away and a row that lingers reads as a click
            // that missed. Following has nothing to add until the
            // server answers with the thread itself.
            ui.store.borrow_mut().unfollow(&root);
            ui.refresh_threads_view();
        }
        ui.commands
            .send(Command::FollowThread {
                team,
                root,
                following,
            })
            .ok();
    });
}

/// Connect folding a category shut and opening it again.
fn wire_categories(ui: &Ui, sidebar: &gtk::ListBox) {
    let folding = ui.clone();
    sidebar.connect_row_activated(move |_, row| {
        // Channel rows are activatable too -- that is how a click opens
        // one -- so this has to say which kind it was handed.
        if row.has_css_class("mmrs-section") {
            folding.toggle_category(&row.widget_name());
            return;
        }
        if row.widget_name().starts_with("mmrs:") {
            return;
        }
        // A click on a channel comes back to the conversation, even
        // when that channel is already the selected one. Opening is the
        // selection handler's job and only fires when the selection
        // *changes*, so clicking the channel you are already on while
        // reading saved messages did nothing at all.
        folding.show_pane(&Pane::Channel);
    });

    // One drop target for the whole list rather than one per row: the
    // interesting question is *where* in the list the pointer let go,
    // and that is a height, which only the list knows.
    let drop = gtk::DropTarget::new(glib::types::Type::STRING, gdk::DragAction::MOVE);
    let ui = ui.clone();
    drop.connect_drop(move |_, value, _, y| {
        let Ok(channel) = value.get::<String>() else {
            return false;
        };
        ui.drop_channel(&ChannelId::new(channel.as_str()), y)
    });
    sidebar.add_controller(drop);
}

/// Connect choosing a channel: what it opens, and what it closes.
fn wire_selection(ui: &Ui, sidebar: &gtk::ListBox) {
    let ui = ui.clone();
    sidebar.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let name = row.widget_name();
        // The two views sit in the same list as the channels and are
        // selected the same way, but they open something else.
        if name == THREADS_ROW {
            ui.show_pane(&Pane::Threads);
            ui.set_title("Threads", "");
            return;
        }
        if name == DRAFTS_ROW {
            ui.show_pane(&Pane::Drafts);
            ui.set_title("Drafts", "");
            return;
        }
        ui.open_channel(&ChannelId::new(name.to_string()));
    });
}

fn wire(
    ui: &Ui,
    refresh: &gtk::Button,
    sidebar: &gtk::ListBox,
    composer: &Composer,
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
    wire_selection(ui, sidebar);
    wire_categories(ui, sidebar);

    {
        let commands = commands.clone();
        let selected = Rc::clone(&ui.selected);
        let into = composer.clone();
        let ui = ui.clone();
        composer.view.add_controller(submit_keys(move || {
            let text = composer_text(&into.view);
            let attach = into.take_attachments();
            // A file on its own is a message: Mattermost sends a post
            // with no text and the attachment under it.
            if text.trim().is_empty() && attach.is_empty() {
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
                    attach,
                })
                .ok();
            into.view.buffer().set_text("");
            // The draft is gone the moment the message is sent: an
            // empty box records as a deletion, here and on the server.
            ui.remember_drafts();
        }));
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
fn render_blocks(doc: &mm_md::Document, emoji: &Emoji) -> Vec<BlockView> {
    if let Some(plain) = doc.plain_text() {
        return vec![BlockView {
            kind: BlockKind::Text,
            text: plain.to_owned(),
            markup: false,
            inlines: Vec::new(),
            emoji: Vec::new(),
            quoted: false,
            cells: Vec::new(),
        }];
    }
    let mut out = Vec::new();
    push_blocks(&doc.blocks, false, &mut out, emoji);
    out
}

/// The pictures for the custom-emoji shortcodes in one run of inlines.
///
/// Asking also *requests* the ones that are missing, which is why this
/// runs when the row is built: a row is built on every redraw, and the
/// answer arriving makes the next redraw's row differ from the one on
/// screen, which is what gets it drawn again.
fn resolve_emoji(inlines: &[mm_md::Inline], emoji: &Emoji) -> Vec<(String, gdk::Texture)> {
    fn walk(inlines: &[mm_md::Inline], emoji: &Emoji, out: &mut Vec<(String, gdk::Texture)>) {
        for inline in inlines {
            match inline {
                mm_md::Inline::Shortcode(name) => {
                    if out.iter().any(|(held, _)| held == name) {
                        continue;
                    }
                    if let Some(texture) = emoji.texture(name) {
                        out.push((name.clone(), texture));
                    }
                }
                mm_md::Inline::Emphasis(inner)
                | mm_md::Inline::Strong(inner)
                | mm_md::Inline::Strike(inner) => walk(inner, emoji, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(inlines, emoji, &mut out);
    out
}

fn push_blocks(blocks: &[mm_md::Block], quoted: bool, out: &mut Vec<BlockView>, emoji: &Emoji) {
    for block in blocks {
        match block {
            mm_md::Block::Paragraph(inlines) | mm_md::Block::Heading { inlines, .. } => {
                out.push(BlockView {
                    kind: BlockKind::Text,
                    text: mm_md::to_pango(inlines),
                    markup: true,
                    inlines: inlines.clone(),
                    emoji: resolve_emoji(inlines, emoji),
                    quoted,
                    cells: Vec::new(),
                });
            }
            mm_md::Block::ListItem(inlines) => out.push(BlockView {
                kind: BlockKind::Bullet,
                text: mm_md::to_pango(inlines),
                markup: true,
                inlines: inlines.clone(),
                emoji: resolve_emoji(inlines, emoji),
                quoted,
                cells: Vec::new(),
            }),
            mm_md::Block::Table(rows) => out.push(BlockView {
                kind: BlockKind::Table,
                text: String::new(),
                markup: true,
                inlines: Vec::new(),
                emoji: Vec::new(),
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
                inlines: Vec::new(),
                emoji: Vec::new(),
                quoted,
                cells: Vec::new(),
            }),
            // A quote inside a quote still draws one bar. Mattermost's own
            // rendering nests them, but a second bar on a chat message is
            // noise and the depth is not information the reader needs.
            mm_md::Block::Quote(inner) => push_blocks(inner, true, out, emoji),
            mm_md::Block::Rule => out.push(BlockView {
                kind: BlockKind::Rule,
                text: String::new(),
                markup: false,
                inlines: Vec::new(),
                emoji: Vec::new(),
                quoted,
                cells: Vec::new(),
            }),
        }
    }
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
fn build_message_tools(row: &gtk::Box, item: &gtk::ListItem, ctx: &RowContext) -> gtk::Box {
    let (commands, store, server, shown) = (&ctx.commands, &ctx.store, &ctx.server, &ctx.shown);
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

    // Shared with the leave handler below: a popover opened from this
    // toolbar must not be taken down by the leave that opening it
    // caused. The same trap the overflow menu is guarded against.
    let picking = Rc::new(std::cell::Cell::new(false));
    tools.append(&any_reaction_button(row, &ctx.emoji, &picking));

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
            save_post(&store, &commands, &post, !already);
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
        let picking = Rc::clone(&picking);
        hover.connect_leave(move |_| {
            if !more.is_active() && !picking.get() {
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

fn setup_post_row(item: &gtk::ListItem, ctx: &RowContext) {
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
    // Everything attached that is not drawn as a picture above: a
    // document, an archive, an image whose size the server never
    // stated. One row each, with somewhere to click to keep it --
    // without which a post whose only content is a file rendered as an
    // author, a timestamp and nothing at all.
    let files = gtk::Box::new(gtk::Orientation::Vertical, 4);
    files.set_margin_top(4);
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
    main.append(&files);
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
        let ctx = ctx.clone();
        first_hover.connect_enter(move |_, _, _| {
            // The overlay's last child is the toolbar once there is one;
            // before that it is the message itself.
            let tools = match floating.last_child().and_downcast::<gtk::Box>() {
                Some(tools) if tools.has_css_class("mmrs-tools") => tools,
                _ => {
                    let tools = build_message_tools(&row, &item, &ctx);
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
                label_quick_reactions(&tools, &data, &ctx.emoji);
                tools.set_visible(true);
                show_only(&ctx.shown, &tools);
            }
        });
    }
    row.add_controller(first_hover);

    // A name and a face are the two places Mattermost opens a person's
    // card from, so both get the gesture and the hand cursor that says
    // they are worth clicking.
    for target in [author.upcast::<gtk::Widget>(), gutter.upcast()] {
        target.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::new();
        let item = item.clone();
        let ctx = ctx.clone();
        let at = target.clone();
        click.connect_released(move |_, _, _, _| {
            show_profile(&at, &item, &ctx.store, &ctx.avatars, &ctx.emoji);
        });
        target.add_controller(click);
    }

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
fn label_quick_reactions(tools: &gtk::Box, data: &PostRow, emoji_cache: &Emoji) {
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
        // One of the server's own draws its picture; everything else
        // draws a glyph, or its shortcode when there is no glyph.
        if let Some(texture) = emoji_cache.texture(&emoji) {
            current.set_child(Some(&Emoji::image(&texture)));
        } else {
            current.set_label(
                &mm_md::glyph(&emoji).map_or_else(|| format!(":{emoji}:"), ToOwned::to_owned),
            );
        }
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

/// One block of body text.
///
/// A `GtkLabel` of Pango markup, unless the block holds a custom emoji
/// this client can actually draw -- then a `GtkTextView`, which is the
/// only widget that can put a picture in the middle of a wrapped line.
///
/// **A label by default, on purpose.** A `TextView` carries a buffer, a
/// tag table and a layout of its own, and these rows are recycled by a
/// `ListView` that realizes one widget set per visible row; making every
/// paragraph one would pay that cost on every message to serve the few
/// that need it. Both paths render the same `Vec<Inline>`, so they
/// cannot disagree about what the message says.
fn body_text(block: &BlockView) -> gtk::Widget {
    if block.emoji.is_empty() {
        return card_label(&block.text, block.markup, &[]).upcast();
    }
    inline_view(&block.inlines, &block.emoji).upcast()
}

/// Draw inlines into a `TextView`, pictures and all.
fn inline_view(inlines: &[mm_md::Inline], emoji: &[(String, gdk::Texture)]) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .hexpand(true)
        // Its own background would be a pale slab behind the message;
        // the CSS turns it off, and this stops it claiming padding too.
        .css_classes(["mmrs-inline"])
        .left_margin(0)
        .right_margin(0)
        .top_margin(0)
        .bottom_margin(0)
        .build();
    let buffer = view.buffer();
    let tags = buffer.tag_table();
    // Named at construction: `name` is construct-only on a `TextTag`,
    // and setting it afterwards aborts the process rather than
    // failing -- the setter panics inside a GTK callback, which cannot
    // unwind.
    for tag in [
        gtk::TextTag::builder().name("b").weight(700).build(),
        gtk::TextTag::builder()
            .name("i")
            .style(gtk::pango::Style::Italic)
            .build(),
        gtk::TextTag::builder()
            .name("s")
            .strikethrough(true)
            .build(),
        gtk::TextTag::builder()
            .name("tt")
            .family("monospace")
            .scale(0.9)
            .build(),
        // Mattermost's own blue, the same one the stylesheet gives a
        // `GtkLabel`'s links -- GTK would otherwise paint these in the
        // desktop's accent colour, which is orange on Ubuntu.
        gtk::TextTag::builder()
            .name("a")
            .foreground("#5d89ea")
            .underline(gtk::pango::Underline::Single)
            .build(),
        // Mattermost's own colours, the same two the markup path uses:
        // everyone else's mention blue, and the reader's own on amber,
        // which is the one piece of colour a chat window must not get
        // wrong.
        gtk::TextTag::builder()
            .name("mention")
            .foreground("#5d89ea")
            .weight(700)
            .build(),
        gtk::TextTag::builder()
            .name("mention-me")
            .background("#ffd470")
            .foreground("#1a1a1a")
            .weight(700)
            .build(),
    ] {
        tags.add(&tag);
    }

    // Where each link goes, by the tag that marks it. One tag per link,
    // because a tag is the only thing a `TextView` can attach meaning
    // to and it cannot carry a payload.
    let hrefs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let mut at = buffer.end_iter();
    write_inlines(&buffer, &mut at, inlines, &[], emoji, &hrefs);
    wire_inline_links(&view, &hrefs);
    view
}

/// Write one run of inlines, carrying the tags that are open around it.
fn write_inlines(
    buffer: &gtk::TextBuffer,
    at: &mut gtk::TextIter,
    inlines: &[mm_md::Inline],
    open: &[String],
    emoji: &[(String, gdk::Texture)],
    hrefs: &Rc<RefCell<Vec<String>>>,
) {
    use mm_md::Inline;
    let names: Vec<&str> = open.iter().map(String::as_str).collect();
    for inline in inlines {
        match inline {
            Inline::Text(text) => buffer.insert_with_tags_by_name(at, text, &names),
            Inline::Code(code) => {
                let mut with = names.clone();
                with.push("tt");
                buffer.insert_with_tags_by_name(at, code, &with);
            }
            Inline::SoftBreak => buffer.insert_with_tags_by_name(at, "\n", &names),
            Inline::Mention { name, is_me } => {
                let mut with = names.clone();
                with.push(if *is_me { "mention-me" } else { "mention" });
                buffer.insert_with_tags_by_name(at, &format!("@{name}"), &with);
            }
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                let which = match inline {
                    Inline::Emphasis(_) => "i",
                    Inline::Strong(_) => "b",
                    _ => "s",
                };
                let mut with: Vec<String> = open.to_vec();
                with.push(which.to_owned());
                write_inlines(buffer, at, inner, &with, emoji, hrefs);
            }
            Inline::Link { href, text } => {
                let which = format!("link-{}", hrefs.borrow().len());
                hrefs.borrow_mut().push(href.clone());
                buffer
                    .tag_table()
                    .add(&gtk::TextTag::builder().name(&which).build());
                let mut with = names.clone();
                with.push("a");
                with.push(&which);
                buffer.insert_with_tags_by_name(at, text, &with);
            }
            Inline::Shortcode(name) => {
                // A picture if there is one, and otherwise the name
                // back with its colons -- the same fallback the markup
                // path has always had.
                let found = emoji
                    .iter()
                    .find_map(|(held, texture)| (held == name).then_some(texture));
                if let Some(texture) = found {
                    // A paintable, not a child widget on an anchor: a
                    // widget occupies the whole line box and leaves a
                    // band of empty space under the text, while a
                    // paintable is laid out as a character is.
                    buffer.insert_paintable(at, texture);
                } else {
                    buffer.insert_with_tags_by_name(at, &format!(":{name}:"), &names);
                }
            }
        }
    }
}

/// What is under the pointer, in buffer coordinates.
///
/// Pointer positions arrive as `f64` and the buffer indexes by whole
/// pixels; a fractional pixel is below the size of a glyph, so the
/// truncation loses nothing a reader could aim with.
fn iter_under(view: &gtk::TextView, x: f64, y: f64) -> Option<gtk::TextIter> {
    let round = |v: f64| {
        let v = v.round();
        if v.is_finite() && v.abs() < f64::from(i32::MAX) {
            #[allow(clippy::cast_possible_truncation)]
            {
                v as i32
            }
        } else {
            0
        }
    };
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, round(x), round(y));
    view.iter_at_location(bx, by)
}

/// Open a link in a `TextView`, which has no `activate-link` of its own.
///
/// Released rather than pressed, and only when the press and the release
/// are on the same link: a drag that starts on a link and ends in the
/// text beside it is somebody selecting, not somebody clicking.
fn wire_inline_links(view: &gtk::TextView, hrefs: &Rc<RefCell<Vec<String>>>) {
    if hrefs.borrow().is_empty() {
        return;
    }
    let click = gtk::GestureClick::new();
    let hrefs = Rc::clone(hrefs);
    let text = view.clone();
    click.connect_released(move |gesture, presses, x, y| {
        if presses != 1 {
            return;
        }
        if text.buffer().has_selection() {
            return;
        }
        let Some(at) = iter_under(&text, x, y) else {
            return;
        };
        let found = at.tags().into_iter().find_map(|tag| {
            let name = tag.name()?;
            let index: usize = name.strip_prefix("link-")?.parse().ok()?;
            hrefs.borrow().get(index).cloned()
        });
        if let Some(href) = found {
            gtk::UriLauncher::new(&href).launch(
                text.root().and_downcast::<gtk::Window>().as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    view.add_controller(click);

    // A hand over a link, the way a label does it.
    let motion = gtk::EventControllerMotion::new();
    let text = view.clone();
    motion.connect_motion(move |_, x, y| {
        let over = iter_under(&text, x, y).is_some_and(|at| {
            at.tags()
                .iter()
                .any(|t| t.name().is_some_and(|n| n.starts_with("link-")))
        });
        text.set_cursor_from_name(Some(if over { "pointer" } else { "text" }));
    });
    view.add_controller(motion);
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
                item.append(&body_text(block));
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
            BlockKind::Text => body_text(block),
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

/// Draw a row for everything attached that is not a picture above.
fn bind_files(
    files: &gtk::Box,
    data: &PostRow,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) {
    while let Some(old) = files.first_child() {
        files.remove(&old);
    }
    let attached = data.files();
    files.set_visible(!attached.is_empty());
    for file in attached {
        files.append(&file_row(&file, commands));
    }
}

/// One attachment: what it is, how big, and a button to keep it.
fn file_row(
    file: &mm_types::FileRef,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.add_css_class("card");
    line.set_halign(gtk::Align::Start);
    line.set_margin_start(2);

    let icon = gtk::Image::from_icon_name(if file.is_image() {
        "image-x-generic-symbolic"
    } else {
        "mail-attachment-symbolic"
    });
    icon.set_margin_start(10);
    icon.add_css_class("dim-label");
    line.append(&icon);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_margin_top(8);
    column.set_margin_bottom(8);
    // Some endpoints omit the metadata, leaving only an id. Saying a
    // file is there beats showing nothing.
    let name = if file.name.is_empty() {
        "attachment"
    } else {
        &file.name
    };
    column.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(44)
            .build(),
    );
    let size = file.human_size();
    if !size.is_empty() {
        column.append(
            &gtk::Label::builder()
                .label(&size)
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    line.append(&column);

    let save = gtk::Button::from_icon_name("document-save-symbolic");
    save.set_tooltip_text(Some("Save to Downloads"));
    save.add_css_class("flat");
    save.set_valign(gtk::Align::Center);
    save.set_margin_end(6);
    let commands = commands.clone();
    let wanted = file.id.clone();
    let as_named = name.to_owned();
    save.connect_clicked(move |_| {
        commands
            .send(Command::SaveFile {
                file: wanted.clone(),
                name: as_named.clone(),
            })
            .ok();
    });
    line.append(&save);
    line
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

/// Save a post, or take it out of the saved list.
///
/// Applied to the store as well as sent, because saving is a
/// **preference** and the server announces a changed preference over a
/// WebSocket event this client does not decode -- so without this the
/// menu goes on offering "Remove from saved" for a post that has just
/// been unsaved, and the next click asks the server to unsave it twice.
fn save_post(
    store: &Rc<RefCell<Store>>,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    post: &PostId,
    saved: bool,
) {
    store.borrow_mut().set_saved_locally(post, saved);
    commands
        .send(Command::Save {
            post: post.clone(),
            saved,
        })
        .ok();
}

/// The overflow menu's save entry, which cannot go through the generic
/// `add` because it has to touch the store as well as the engine.
fn wire_save_entry(
    button: &gtk::Button,
    popover: &gtk::Popover,
    store: &Rc<RefCell<Store>>,
    commands: &tokio::sync::mpsc::UnboundedSender<Command>,
    post: &PostId,
    saved: bool,
) {
    let store = Rc::clone(store);
    let commands = commands.clone();
    let popover = popover.clone();
    let post = post.clone();
    button.connect_clicked(move |_| {
        save_post(&store, &commands, &post, saved);
        popover.popdown();
    });
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
    let button = entry("bookmark-new-symbolic", saving);
    wire_save_entry(&button, popover, store, commands, &post, !saved);
    actions.append(&button);
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
    near: &impl IsA<gtk::Widget>,
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
fn bind_message_tools(floating: &gtk::Overlay, data: &PostRow, emoji: &Emoji) {
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
        label_quick_reactions(&tools, data, emoji);
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
    files: gtk::Box,
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
        let files = images.next_sibling().and_downcast::<gtk::Box>()?;
        let reactions = files.next_sibling().and_downcast::<gtk::Box>()?;
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
            files,
            reactions,
        })
    }
}

fn bind_post_row(item: &gtk::ListItem, ctx: &RowContext, relative: bool) {
    let (commands, viewer, links) = (&ctx.commands, &ctx.viewer, &ctx.links);
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
        files,
        reactions,
    }) = RowWidgets::find(item)
    else {
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
    set_presence(&dot, data.presence(), !grouped);
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

    bind_message_tools(&floating, &data, &ctx.emoji);

    bind_cards(&cards, &data, commands, links);
    bind_images(&images, &data, commands, viewer);

    bind_files(&files, &data, commands);

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
        reactions.append(&reaction_chip(&data.id(), &chip, &ctx.emoji));
    }
}

/// How big the edit window opens: a few lines of message, not a page.
const EDITOR_WIDTH: i32 = 560;
const EDITOR_HEIGHT: i32 = 260;

/// How many people the mention popup offers before the reader should
/// type another letter rather than scroll a list.
const MENTIONS_OFFERED: usize = 8;

/// How many emoji a half-typed `:` offers. Fewer than the picker's
/// grid, because this is a list with a shortcode on every row and it
/// sits over the message being written.
const SHORTCODES_OFFERED: usize = 10;

/// How long the typing has to stop before a draft is written down.
/// Long enough not to be a request per word, short enough that what
/// was typed survives walking away from the machine.
const DRAFT_IDLE: std::time::Duration = std::time::Duration::from_secs(2);

/// How often "somebody is typing" is sent while the typing goes on.
/// The server forgets one after five seconds, so this has to be under
/// that for the indicator to stay up.
const TYPING_EVERY: std::time::Duration = std::time::Duration::from_secs(4);
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

/// How often presence is asked for again. Mattermost's own webapp uses
/// a minute; a dot a minute out of date is nobody's problem, and
/// anything faster is a request a minute for decoration.
const PRESENCE_REPOLL: std::time::Duration = std::time::Duration::from_secs(60);
/// How tall the composer grows before it starts scrolling: about six
/// lines, after which the conversation would start losing the window.
const COMPOSER_MAX_HEIGHT: i32 = 160;
/// The tag carrying the red squiggle, by the name its buffer knows it by.
const MISSPELLED: &str = "mmrs-misspelled";
/// How long the typing has to stop before the dictionary is asked.
///
/// Checking on every keystroke would run both dictionaries over the
/// whole message once per letter of it, and mark a word wrong while it
/// is still half written, which is the thing that makes spell-checking
/// feel like being nagged.
const SPELL_DELAY: std::time::Duration = std::time::Duration::from_millis(400);
/// How many replacements the right-click menu offers.
const SUGGESTIONS_OFFERED: usize = 6;
/// How wide the channel menu is, and how tall its member list grows
/// before it scrolls.
const CHANNEL_MENU_WIDTH: i32 = 320;
const CHANNEL_MENU_HEIGHT: i32 = 420;
/// The emoji picker: how many it offers at once, and how big the grid
/// is. Capped because the table is three thousand long and nobody
/// scrolls that far -- the search box is the way to the rest.
const EMOJI_OFFERED: usize = 180;
const EMOJI_COLUMNS: u32 = 8;
const EMOJI_WIDTH: i32 = 320;
const EMOJI_HEIGHT: i32 = 280;
const PREVIEW_WIDTH: i32 = 520;
const PREVIEW_IMAGE: i32 = 80;
const MENTIONS_WIDTH: i32 = 340;
const MENTIONS_HEIGHT: i32 = 260;

/// The quick switcher's shape, and how many channels it offers before
/// the reader should type another letter rather than scroll.
const SWITCHER_WIDTH: i32 = 520;
const SWITCHER_HEIGHT: i32 = 460;
/// How much has to be typed before the server is asked. One letter
/// matches most of a server, and the answer would be noise.
const ELSEWHERE_MINIMUM: usize = 2;
/// What the badge count starts at: not zero, so the first pass always
/// says something. A client that starts with no mentions still has to
/// clear a badge the last run left on the dock.
const NO_BADGE_YET: i64 = -1;
/// How long a `--hidden` start waits for a tray icon before deciding
/// there is not going to be one and showing itself anyway.
const HIDDEN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// This binary's own flag, which GTK must never see: it parses argv
/// itself and exits on an option it does not know.
const HIDDEN_FLAG: &str = "--hidden";
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
    emoji: &Emoji,
) -> crate::row::AttachmentView {
    let mut doc = mm_md::parse(&attachment.text);
    mm_md::mark_mentions(&mut doc, &|name| store.mentioned(name));
    crate::row::AttachmentView {
        blocks: render_blocks(&doc, emoji),
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
    /// Search results, pinned messages or saved messages -- one page,
    /// told apart by which list it is drawing.
    Found(mm_core::Found),
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
fn reaction_chip(post: &str, summary: &mm_core::ReactionSummary, emoji: &Emoji) -> gtk::Button {
    let commands = &emoji.commands;
    let chip = gtk::Button::builder()
        .css_classes(["flat", "caption"])
        .tooltip_text(format!(":{}:", summary.emoji))
        .build();
    // One of the server's own, with its picture here: the chip is the
    // image and the count side by side. Otherwise a label -- the glyph
    // for a standard shortcode, and the shortcode itself for a custom
    // one whose picture has not arrived or never will, because
    // `:shipit:` says what was meant where a blank chip says nothing.
    if let Some(texture) = emoji.texture(&summary.emoji) {
        let both = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        both.append(&Emoji::image(&texture));
        both.append(&gtk::Label::new(Some(&summary.count.to_string())));
        chip.set_child(Some(&both));
    } else {
        let face = mm_md::glyph(&summary.emoji)
            .map_or_else(|| format!(":{}:", summary.emoji), ToOwned::to_owned);
        chip.set_label(&format!("{face} {}", summary.count));
    }
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
    ctx: &RowContext,
    // `relative`: say how long ago each message was instead of what the
    // clock said. The thread pane has no day separators, so a bare
    // "17:01" there could be today or a fortnight ago.
    relative: bool,
) -> (gio::ListStore, gtk::ListView, gtk::ScrolledWindow) {
    let posts = gio::ListStore::new::<PostRow>();
    let factory = gtk::SignalListItemFactory::new();

    let for_setup = ctx.clone();
    factory.connect_setup(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            setup_post_row(item, &for_setup);
        }
    });
    let for_bind = ctx.clone();
    factory.connect_bind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            // This widget may have been showing the previous occupant's
            // toolbar. The message under it has just changed, so the
            // toolbar is wrong whatever the pointer is doing.
            hide_tools(item, &for_bind.shown);
            bind_post_row(item, &for_bind, relative);
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
                // The sync rebuilds the sidebar but fetches no posts,
                // so without this the conversation on screen keeps
                // whatever it had when the socket died -- which, after
                // a suspended laptop, is everything except what was
                // said while it slept. Asking for the channel again
                // fetches the newest page, and the store keeps that
                // fresh tail even when it cannot reach what is held.
                if let Some(open) = self.selected.borrow().clone() {
                    self.commands.send(Command::OpenChannel(open)).ok();
                }
            }
            Event::TrayReady => self.has_tray.set(true),
            Event::Present => {
                self.window.set_visible(true);
                self.window.present();
            }
            Event::Quit => {
                // Through the window rather than `app.quit()`, so that
                // the close handler still writes down the size and the
                // channel -- and it has to be told not to hide this
                // time, which is what clearing the flag does.
                self.has_tray.set(false);
                self.window.close();
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
        // The reader's own account settings, which this used to
        // default to "tell me about everything" -- so an account
        // asking for mentions only was notified about every message
        // in every unmuted channel.
        let mine = store.users.get(&me);
        let prop = |key: &str| -> &str {
            mine.and_then(|u| u.notify_props.get(key))
                .map_or("", String::as_str)
        };
        let settings = mm_desktop::NotifySettings {
            global: mm_desktop::DesktopNotify::from_prop(prop("desktop")),
            channel: member.map_or(mm_desktop::DesktopNotify::Inherit, |m| {
                mm_desktop::DesktopNotify::from_prop(
                    m.notify_props.get("desktop").map_or("", String::as_str),
                )
            }),
            muted: store.is_muted(&post.channel_id),
            // Mattermost writes this as the string "true" or "false"
            // and treats anything missing as on.
            sound: prop("desktop_sound") != "false",
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
    /// A reply to a quick-switcher query, or the channel it led to.
    fn on_elsewhere(&self, event: Event) {
        match event {
            Event::Elsewhere {
                term,
                channels,
                people,
            } => {
                *self.elsewhere.borrow_mut() = Elsewhere {
                    term,
                    channels,
                    people,
                };
                // Only if a switcher is still open to draw them in.
                let refill = self.refill_switcher.borrow().clone();
                if let Some(refill) = refill {
                    refill();
                }
            }
            Event::Joined { channel, member } => {
                let id = channel.id.clone();
                {
                    let mut store = self.store.borrow_mut();
                    store.apply_channels(vec![channel]);
                    store.apply_members(vec![member]);
                }
                self.go_to_new_channel(&id);
            }
            Event::Opened(channel) => self.go_to_new_channel(&channel),
            // Every other variant has its own arm in `on_data`.
            _ => {}
        }
    }

    /// Show a channel that has only just appeared in the sidebar.
    fn go_to_new_channel(&self, channel: &ChannelId) {
        self.set_status("");
        // In front of the reader: this is reached by joining, by
        // following a link, and by clicking a notification, and every
        // one of those is somebody asking to be taken there now.
        self.window.present();
        // Drawn now rather than on the next coalesced pass: the row has
        // to exist before `show_channel` can select it.
        self.sidebar_stale.set(true);
        self.refresh_sidebar();
        self.show_channel(channel);
    }

    /// Ask the server about a quick-switcher query.
    ///
    /// Only once there is something to ask about: one letter matches
    /// most of a server, and asking on every keystroke of a long name
    /// would be a request per letter for answers nobody reads.
    fn ask_elsewhere(&self, query: &str) {
        let term = query.trim().to_owned();
        if term.chars().count() < ELSEWHERE_MINIMUM {
            *self.elsewhere.borrow_mut() = Elsewhere::default();
            return;
        }
        let Some(where_to_look) = self.store.borrow().teams.first().map(|t| t.id.clone()) else {
            return;
        };
        self.commands
            .send(Command::Elsewhere {
                team: where_to_look,
                term,
            })
            .ok();
    }

    /// Draw what the server found under what the client already knew.
    ///
    /// Channels the reader is already in are left out: they are in the
    /// list above, and offering them twice makes the switcher look
    /// broken. People already in the list are not, because a person is
    /// not a channel -- the row above is the conversation, this one is
    /// the human.
    fn append_elsewhere(&self, list: &gtk::ListBox, query: &str) {
        let found = self.elsewhere.borrow();
        // An answer to a query the reader has already typed past.
        if found.term != query.trim() || query.trim().is_empty() {
            return;
        }
        let store = self.store.borrow();
        let unjoined: Vec<&mm_types::Channel> = found
            .channels
            .iter()
            .filter(|c| !store.my_member.contains_key(&c.id))
            .take(SWITCHER_RESULTS)
            .collect();
        if !unjoined.is_empty() {
            list.append(&mention_heading("NOT JOINED"));
            for channel in unjoined {
                list.append(&elsewhere_row(
                    &format!("join:{}", channel.id),
                    "#",
                    &channel.display_name,
                    "",
                ));
            }
        }

        let people: Vec<&mm_types::User> = found
            .people
            .iter()
            .filter(|u| u.id != store.me && !u.is_deactivated())
            .take(SWITCHER_RESULTS)
            .collect();
        if !people.is_empty() {
            list.append(&mention_heading("PEOPLE"));
            for person in people {
                list.append(&elsewhere_row(
                    &format!("dm:{}", person.id),
                    "@",
                    &format!("@{}", person.username),
                    &person.full_name(),
                ));
            }
        }
    }

    /// Act on whatever row the switcher was on when Return was pressed.
    ///
    /// The row's name says what it is: a bare channel id is one the
    /// reader is already in, `join:` is one they are not, and `dm:` is
    /// a person rather than a place.
    fn switcher_chose(&self, name: &str) {
        if let Some(channel) = name.strip_prefix("join:") {
            self.set_status("joining…");
            self.commands
                .send(Command::JoinChannel(ChannelId::new(channel)))
                .ok();
        } else if let Some(user) = name.strip_prefix("dm:") {
            self.set_status("opening…");
            self.commands
                .send(Command::OpenDirect(UserId::new(user)))
                .ok();
        } else if !name.is_empty() {
            self.show_channel(&ChannelId::new(name));
        }
    }

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
                ui.append_elsewhere(&list, query);
                // So Return goes somewhere without arrowing down first.
                if let Some(first) = first_selectable(&list, 0, 1) {
                    list.select_row(Some(&first));
                }
            }
        };
        fill("");
        {
            let fill = fill.clone();
            let ui = self.clone();
            entry.connect_search_changed(move |entry| {
                let query = entry.text().to_string();
                fill(&query);
                ui.ask_elsewhere(&query);
            });
        }
        // So an answer that lands after the reader has stopped typing
        // redraws the list it belongs to. Cleared when the window goes,
        // which is what keeps a late answer from reaching a dead one.
        {
            let fill = fill.clone();
            let entry = entry.clone();
            *self.refill_switcher.borrow_mut() = Some(Rc::new(move || fill(entry.text().as_str())));
        }
        {
            let ui = self.clone();
            window.connect_close_request(move |_| {
                *ui.refill_switcher.borrow_mut() = None;
                glib::Propagation::Proceed
            });
        }

        let go = {
            let ui = self.clone();
            let window = window.clone();
            move |row: &gtk::ListBoxRow| {
                let name = row.widget_name();
                window.close();
                ui.switcher_chose(name.as_str());
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
    /// An answer list came back: search results, pinned or saved.
    ///
    /// Search takes the reader to its page, because they asked a
    /// question and are waiting for it. The other two only redraw, and
    /// only if that page is still the one on show -- the button that
    /// asked already moved them there.
    fn on_found(&self, event: Event) {
        let which = match event {
            Event::SearchResults {
                terms,
                page_no,
                full,
                page,
            } => {
                let nothing_more = page_no > 0 && page.order.is_empty();
                self.store
                    .borrow_mut()
                    .apply_search(&terms, &page, page_no > 0);
                self.search_page.set(page_no);
                self.search_more.set(full);
                // Said out loud rather than letting the button quietly
                // vanish. On a server without Elasticsearch the
                // database search caps at one page and answers the
                // second with nothing, and a button that disappears
                // having done nothing reads as a bug.
                self.set_status(if nothing_more { "no more results" } else { "" });
                // Already on the page when this is a later one, and
                // `show_pane` redraws it either way.
                self.show_pane(&Pane::Found(mm_core::Found::Search));
                return;
            }
            Event::PinnedPosts { channel, page } => {
                self.store.borrow_mut().apply_pinned(&channel, &page);
                mm_core::Found::Pinned(channel)
            }
            Event::SavedPosts(page) => {
                self.store.borrow_mut().apply_saved(&page);
                mm_core::Found::Saved
            }
            // Every other variant has its own arm in `on_data`.
            _ => return,
        };
        if *self.pane.borrow() == Pane::Found(which.clone()) {
            self.refresh_found(&which);
        }
    }

    /// What the open channel is and who is in it.
    ///
    /// Everything here is already held: the header line came with the
    /// channel and the roster was fetched when it opened, so the menu
    /// opens on the click rather than after a round trip.
    fn open_channel_menu(&self, at: &gtk::Widget) {
        let Some(channel) = self.selected.borrow().clone() else {
            return;
        };
        let store = self.store.borrow();
        let heading = store.channel_heading(&channel);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(14);
        column.set_margin_bottom(14);
        column.set_size_request(CHANNEL_MENU_WIDTH, -1);
        column.append(
            &gtk::Label::builder()
                .label(&heading.name)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["title-4"])
                .build(),
        );
        if !heading.topic.is_empty() {
            column.append(
                &gtk::Label::builder()
                    .label(&heading.topic)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }

        column.append(&self.mute_switch(&channel));
        column.append(&self.notify_level(&channel));

        let people = store.channel_roster(&channel);
        column.append(
            &gtk::Label::builder()
                .label(match people.len() {
                    0 => "MEMBERS".to_owned(),
                    // A DM with yourself really is one member, and
                    // "1 MEMBERS" reads as a bug in the client.
                    1 => "1 MEMBER".to_owned(),
                    n => format!("{n} MEMBERS"),
                })
                .xalign(0.0)
                .margin_top(12)
                .css_classes(["dim-label", "caption-heading"])
                .build(),
        );
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        for user in &people {
            list.append(&self.member_row(user));
        }
        if people.is_empty() {
            // The roster arrives with the channel's first page, so this
            // is "not yet" rather than "nobody".
            list.append(&empty_view("Not loaded yet"));
        }
        column.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(CHANNEL_MENU_HEIGHT)
                .build(),
        );

        let menu = gtk::Popover::builder().child(&column).build();
        menu.set_parent(at);
        menu.connect_closed(gtk::Popover::unparent);
        menu.popup();
    }

    /// The mute row: a switch, and a line saying what muting does.
    ///
    /// Worth spelling out, because muting is not silence -- a direct
    /// mention still shows a badge. What goes away is the unread
    /// marker, which is the whole point in a channel of alerts.
    fn mute_switch(&self, channel: &ChannelId) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(12);
        let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.set_hexpand(true);
        words.append(&gtk::Label::builder().label("Mute").xalign(0.0).build());
        words.append(
            &gtk::Label::builder()
                .label("No unread marker. Mentions still show.")
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        row.append(&words);

        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        switch.set_active(self.store.borrow().is_muted(channel));
        let ui = self.clone();
        let channel = channel.clone();
        switch.connect_state_set(move |_, wanted| {
            let changed = ui.store.borrow_mut().set_muted(&channel, wanted);
            if !changed.is_empty() {
                ui.commands
                    .send(Command::SetMuted {
                        channel: channel.clone(),
                        muted: wanted,
                    })
                    .ok();
                ui.sidebar_stale.set(true);
                ui.sidebar_soon();
            }
            glib::Propagation::Proceed
        });
        row.append(&switch);
        row
    }

    /// How loudly this channel notifies.
    ///
    /// The four the server understands, drawn as a row of linked
    /// buttons rather than a dropdown: there are four, they are short,
    /// and which one is on should be readable without opening
    /// anything. "Default" is not a fifth state -- it is the channel
    /// saying nothing and the account's own setting deciding.
    fn notify_level(&self, channel: &ChannelId) -> gtk::Box {
        const LEVELS: &[(&str, &str)] = &[
            ("default", "Default"),
            ("all", "All"),
            ("mention", "Mentions"),
            ("none", "Nothing"),
        ];

        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.set_margin_top(12);
        row.append(
            &gtk::Label::builder()
                .label("Notify me about")
                .xalign(0.0)
                .build(),
        );

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("linked");
        let now = self.store.borrow().channel_notify(channel);
        let mut first: Option<gtk::ToggleButton> = None;
        for (value, label) in LEVELS {
            let button = gtk::ToggleButton::builder()
                .label(*label)
                .active(now == *value)
                .build();
            // Grouped so they behave as one choice rather than four
            // switches; the first one is the group.
            match &first {
                Some(group) => button.set_group(Some(group)),
                None => first = Some(button.clone()),
            }
            let ui = self.clone();
            let channel = channel.clone();
            let value = (*value).to_owned();
            button.connect_toggled(move |button| {
                // Grouping fires the handler for the one going off as
                // well as the one coming on.
                if !button.is_active() {
                    return;
                }
                if ui
                    .store
                    .borrow_mut()
                    .set_channel_notify(&channel, &value)
                    .is_empty()
                {
                    return;
                }
                ui.commands
                    .send(Command::SetChannelNotify {
                        channel: channel.clone(),
                        desktop: value.clone(),
                    })
                    .ok();
            });
            buttons.append(&button);
        }
        row.append(&buttons);
        row
    }

    /// One person in the member list: their face, their name, their dot.
    fn member_row(&self, user: &mm_types::User) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.set_margin_start(6);
        line.set_margin_end(6);
        line.set_margin_top(3);
        line.set_margin_bottom(3);
        let (gutter, face, dot) = face_with_dot(24);
        face.set_text(Some(user.display_name()));
        face.set_custom_image(self.avatars.borrow().get(&user.id));
        set_presence(&dot, self.store.borrow().presence(&user.id), true);
        line.append(&gutter);
        line.append(
            &gtk::Label::builder()
                .label(format!("@{}", user.username))
                .xalign(0.0)
                .build(),
        );
        let full = user.full_name();
        if !full.is_empty() {
            line.append(
                &gtk::Label::builder()
                    .label(&full)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        gtk::ListBoxRow::builder().child(&line).build()
    }

    /// Go to a message found somewhere that is not its channel.
    ///
    /// The channel, and then the message's own thread beside it. Not a
    /// scroll to the post: the message may be a year back, and the
    /// timeline would have to grow a segment around it -- which is real
    /// work in the part of the store that must not be got wrong.
    /// Opening the thread puts the message itself on screen, with
    /// whatever was said in reply, which is what the reader clicked for.
    fn go_to_post(&self, post: &PostId) {
        let found = {
            let store = self.store.borrow();
            store
                .posts
                .get(post)
                .map(|p| (p.channel_id.clone(), p.thread_root().clone()))
        };
        let Some((channel, root)) = found else {
            return;
        };
        self.show_channel(&channel);
        self.commands.send(Command::OpenThread(root)).ok();
    }

    /// Tell the dock how many mentions are waiting, when it changes.
    ///
    /// Only on a change: the sidebar is redrawn on every event worth
    /// redrawing for, and a D-Bus signal per keystroke in a busy
    /// channel would be a lot of noise for a number that did not move.
    fn refresh_badge(&self) {
        let mentions = self.store.borrow().total_mentions();
        if self.badge.replace(mentions) != mentions {
            self.commands.send(Command::Badge(mentions)).ok();
        }
    }

    /// Put the thread panel away.
    ///
    /// The root goes too, not just the panel: a hidden panel still
    /// holding a thread would take the next reply to it.
    fn close_thread(&self) {
        // Before the root goes: that is what says which thread the
        // half-typed reply belongs to.
        self.remember_drafts();
        *self.thread_root.borrow_mut() = None;
        self.thread_composer.view.buffer().set_text("");
        self.thread_posts.remove_all();
        self.thread_pane.set_visible(false);
    }

    /// Move to the channel before or after this one in the sidebar.
    ///
    /// Over the rows rather than over the store, because the sidebar is
    /// what the reader is looking at: it is already in their order,
    /// already folded where they folded it, and already filtered if
    /// they turned the unread filter on. Walking the store instead
    /// would step into channels that are not on screen.
    ///
    /// Wraps around, which is what makes Alt+Shift+Up usable as "the
    /// other unread one" when there are two.
    fn step_channel(&self, forward: bool, unread_only: bool) {
        let rows: Vec<gtk::ListBoxRow> = (0..)
            .map_while(|i| self.sidebar.row_at_index(i))
            .filter(|row| row.is_selectable() && !row.widget_name().starts_with("mmrs:"))
            .collect();
        if rows.is_empty() {
            return;
        }
        let open = self.selected.borrow().clone();
        let at = open.and_then(|open| {
            rows.iter()
                .position(|row| row.widget_name() == open.as_str())
        });
        let unread: std::collections::HashSet<ChannelId> = if unread_only {
            self.store
                .borrow()
                .sidebar()
                .into_iter()
                .filter(mm_core::SidebarRow::is_unread)
                .map(|row| row.channel)
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // From where we are, once around and no further: without the
        // bound, "next unread" in a channel list with nothing unread
        // would spin.
        let count = rows.len();
        let start = at.unwrap_or(if forward { count - 1 } else { 0 });
        for step in 1..=count {
            let next = if forward {
                (start + step) % count
            } else {
                (start + count - step % count) % count
            };
            let row = &rows[next];
            if unread_only && !unread.contains(&ChannelId::new(row.widget_name().as_str())) {
                continue;
            }
            self.sidebar.select_row(Some(row));
            return;
        }
    }

    /// Go where a `mattermost://` link points.
    ///
    /// Held rather than acted on immediately when it cannot be
    /// resolved yet: a link that launches the application arrives
    /// before the channel list does, and a client that said "no such
    /// channel" half a second before learning about it would be wrong
    /// for no reason. Retried on every sidebar redraw, like the channel
    /// being restored, and it wins over that -- the reader asked for
    /// this one just now.
    fn follow_link(&self, uri: &str) {
        match mm_desktop::parse_deep_link(uri) {
            Ok(mm_desktop::DeepLink::Server { host }) => {
                // Nowhere in particular: just this server's window,
                // which is already the one in front of them.
                self.set_status(&format!("link to {host}"));
            }
            Ok(mm_desktop::DeepLink::Permalink { post, .. }) => {
                self.set_status("following link…");
                self.commands.send(Command::FollowPermalink(post)).ok();
            }
            Ok(mm_desktop::DeepLink::Channel { channel, .. }) => {
                *self.pending_link.borrow_mut() = Some(channel);
                if !self.follow_pending_link() {
                    self.set_status("looking for that channel…");
                }
            }
            Err(e) => self.set_status(&format!("link: {e}")),
        }
    }

    /// Try the held link again. Answers whether it landed.
    fn follow_pending_link(&self) -> bool {
        let Some(name) = self.pending_link.borrow().clone() else {
            return false;
        };
        let found = self
            .store
            .borrow()
            .channel_named(&name)
            .map(|channel| channel.id.clone());
        let Some(channel) = found else {
            return false;
        };
        *self.pending_link.borrow_mut() = None;
        // A link is the reader asking for this channel now, so it also
        // settles the question of where they were last time.
        *self.reopen.borrow_mut() = None;
        self.set_status("");
        self.show_channel(&channel);
        true
    }

    /// Put the reader back in the channel they left, once it is there
    /// to put them in.
    ///
    /// Answers whether it happened, so the caller can stop. A choice
    /// the reader made themselves is never overridden; the
    /// first-channel fallback's is, because it was only a stand-in for
    /// not knowing yet.
    fn restore_reopen(&self) -> bool {
        let Some(want) = self.reopen.borrow().clone() else {
            return false;
        };
        if self.selected.borrow().is_some() && !self.provisional.get() {
            // The reader has chosen for themselves since.
            *self.reopen.borrow_mut() = None;
            return false;
        }
        let Some(row) = (0..)
            .map_while(|i| self.sidebar.row_at_index(i))
            .find(|row| row.widget_name() == want.as_str())
        else {
            // Not in the sidebar yet, or not any more. Either way, try
            // again on the next redraw rather than giving up here.
            return false;
        };
        *self.reopen.borrow_mut() = None;
        self.provisional.set(false);
        self.sidebar.select_row(Some(&row));
        true
    }

    /// Write down how the window is right now: its size, and where the
    /// reader is.
    ///
    /// Called when the window closes **and** whenever the channel
    /// changes, not only on close. A close handler alone loses
    /// everything to a crash, a `kill`, or a session ending without
    /// the window being closed -- and the whole point is to come back
    /// where they were.
    fn remember_state(&self) {
        // `default_width` tracks the current size of a resizable
        // window, and while maximized it holds the size to go back to,
        // which is exactly what should be restored later.
        (self.remember)(mm_desktop::Restored {
            geometry: mm_desktop::Geometry {
                width: self.window.default_width(),
                height: self.window.default_height(),
                maximized: self.window.is_maximized(),
            },
            channel: self
                .selected
                .borrow()
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        });
    }

    /// Open the search page and put the caret in the box.
    fn open_search(&self) {
        self.show_pane(&Pane::Found(mm_core::Found::Search));
        self.search_entry.grab_focus();
    }

    /// Draw whichever answer list the page is showing.
    ///
    /// The rows are the same shape the Threads and Drafts views use --
    /// who said it, where, when, and the message -- because a result is
    /// a place to go, and that is what those rows are for.
    fn refresh_found(&self, which: &mm_core::Found) {
        while let Some(old) = self.found_list.first_child() {
            self.found_list.remove(&old);
        }
        let store = self.store.borrow();
        let posts = store.found(which);
        let words = if *which == mm_core::Found::Search {
            mm_md::search_words(&store.search_terms)
        } else {
            Vec::new()
        };
        self.found_title
            .set_text(&found_heading(&store, which, posts.len()));
        self.search_entry
            .set_visible(*which == mm_core::Found::Search);

        for post in &posts {
            let (name, _) = store.post_author(post);
            let where_said = store
                .channels
                .get(&post.channel_id)
                .map_or_else(String::new, |c| store.channel_label(c));
            let row = view_row(&ViewRow {
                who: &name,
                chip: &where_said,
                ago: &describe_ago(post.create_at),
                preview: &mm_md::parse(&post.message).to_plain(),
                footer: "",
                faces: Vec::new(),
                face_names: Vec::new(),
                unread: false,
                // Only on a search, and only the words it really
                // looked for: the query language rides in the same
                // string, and bolding `from` in every hit would be
                // worse than bolding nothing.
                highlight: words.clone(),
                action: self.take_off_list(which, &post.id),
            });
            // The post's own id: its channel and its thread are both
            // questions the store can answer from it, and one id cannot
            // disagree with itself the way two could.
            row.set_widget_name(post.id.as_str());
            self.found_list.append(&row);
        }
        if posts.is_empty() {
            self.found_list.append(&empty_view(match which {
                mm_core::Found::Search if store.search_terms.is_empty() => {
                    "Type a search and press Return"
                }
                mm_core::Found::Search => "Nothing matched",
                mm_core::Found::Saved => "Nothing saved",
                mm_core::Found::Pinned(_) => "Nothing pinned in this channel",
            }));
        } else if *which == mm_core::Found::Search && self.search_more.get() {
            drop(store);
            self.found_list.append(&self.more_results_row());
        }
    }

    /// The row at the foot of a full page of search results.
    ///
    /// A button rather than scrolling to the end, because the server
    /// states no total: a full page is the only evidence that there is
    /// another one, and "load more" that turns out to find nothing is
    /// a better lie than a scrollbar that promises a length it does not
    /// know.
    fn more_results_row(&self) -> gtk::ListBoxRow {
        let button = gtk::Button::builder()
            .label("Load more results")
            .css_classes(["flat"])
            .margin_top(6)
            .margin_bottom(10)
            .build();
        let ui = self.clone();
        button.connect_clicked(move |button| {
            let (terms, team) = {
                let store = ui.store.borrow();
                let Some(team) = store.teams.first().map(|t| t.id.clone()) else {
                    return;
                };
                (store.search_terms.clone(), team)
            };
            // Insensitive rather than removed: the row is rebuilt when
            // the answer lands, and a button that stays clickable
            // meanwhile asks for the same page twice.
            button.set_sensitive(false);
            ui.set_status("searching…");
            ui.commands
                .send(Command::Search {
                    team,
                    terms,
                    page: ui.search_page.get() + 1,
                })
                .ok();
        });
        gtk::ListBoxRow::builder()
            .child(&button)
            .activatable(false)
            .selectable(false)
            .build()
    }

    /// The button that takes a post off the list it is being shown in.
    ///
    /// Only on the pinned and saved lists, where the list **is** the
    /// thing being changed -- a search result has nothing to be taken
    /// off. Until now the only way to unpin was to find the message in
    /// its channel, which is the one place the pinned list exists to
    /// save you going.
    fn take_off_list(&self, which: &mm_core::Found, post: &PostId) -> Option<gtk::Button> {
        let (icon, tip, command) = match which {
            mm_core::Found::Search => return None,
            mm_core::Found::Pinned(_) => (
                "view-pin-symbolic",
                "Unpin from this channel",
                Command::Pin {
                    post: post.clone(),
                    pinned: false,
                },
            ),
            mm_core::Found::Saved => (
                "user-bookmarks-symbolic",
                "Remove from saved",
                Command::Save {
                    post: post.clone(),
                    saved: false,
                },
            ),
        };
        let button = gtk::Button::from_icon_name(icon);
        button.add_css_class("flat");
        button.set_tooltip_text(Some(tip));
        // Or the click reaches the row underneath and opens the message
        // that is being taken off the list.
        button.set_can_focus(false);
        let ui = self.clone();
        button.connect_clicked(move |_| {
            // Unsaving goes through `save_post`, which also records it
            // in the store: a saved post is a preference, and the
            // server announces that over an event this client does not
            // decode.
            if let Command::Save { post, saved } = &command {
                save_post(&ui.store, &ui.commands, post, *saved);
            } else {
                ui.commands.send(command.clone()).ok();
            }
            // The server answers a pin with a WebSocket event about the
            // *post*, not about the list, so nothing would redraw this
            // page. Ask for the list again, which is the only honest
            // way to know what is on it.
            ui.refetch_found();
        });
        Some(button)
    }

    /// Ask the server for whichever list is on screen.
    fn refetch_found(&self) {
        let Pane::Found(which) = self.pane.borrow().clone() else {
            return;
        };
        match which {
            mm_core::Found::Pinned(channel) => {
                self.commands.send(Command::Pinned(channel)).ok();
            }
            mm_core::Found::Saved => {
                self.commands.send(Command::Saved).ok();
            }
            mm_core::Found::Search => {}
        }
    }

    fn show_pane(&self, pane: &Pane) {
        *self.pane.borrow_mut() = pane.clone();
        match pane {
            Pane::Channel => {
                self.middle.set_visible_child_name("channel");
                self.composer_pane.set_visible(true);
                if let Some(open) = self.selected.borrow().clone() {
                    self.refresh_heading(&open);
                }
            }
            Pane::Threads => {
                self.middle.set_visible_child_name("threads");
                // Nothing to send to: these views are lists of places.
                self.composer_pane.set_visible(false);
                self.refresh_threads_view();
            }
            Pane::Drafts => {
                self.middle.set_visible_child_name("drafts");
                self.composer_pane.set_visible(false);
                self.refresh_drafts_view();
            }
            Pane::Found(which) => {
                self.middle.set_visible_child_name("found");
                self.composer_pane.set_visible(false);
                self.refresh_found(which);
            }
        }
    }

    /// Go to a channel from somewhere that is not the sidebar -- a draft,
    /// so far. Selecting the row is what opens it: the sidebar's own
    /// handler does the fetching and the mark-read, and going around it
    /// would mean two places that know how to open a channel.
    fn show_channel(&self, channel: &ChannelId) {
        // The channel first, the sidebar after. It used to be the other
        // way round -- find the row, select it, and let the selection
        // handler do the work -- and a channel with no row could not be
        // opened at all: the quick switcher found one in a folded
        // category, selected nothing, and the window stayed where it
        // was with no sign anything had happened.
        self.open_channel(channel);
        self.unfold_category_for(channel);
        self.show_pane(&Pane::Channel);
        // Rebuilt because folding may have changed what is in it, and
        // because the unread filter hides a channel that is caught up
        // until it is the open one -- which it now is.
        self.sidebar_stale.set(true);
        self.refresh_sidebar();
        let row = (0..)
            .map_while(|i| self.sidebar.row_at_index(i))
            .find(|row| row.widget_name() == channel.as_str());
        if let Some(row) = row {
            self.sidebar.select_row(Some(&row));
        }
    }

    /// Open the category a channel is in, if it is folded shut.
    ///
    /// Going somewhere the reader cannot then see in the sidebar is
    /// disorienting: the conversation changes and nothing says where
    /// they are. Mattermost unfolds it too, and it tells the server, so
    /// the fold follows them to their other clients.
    fn unfold_category_for(&self, channel: &ChannelId) {
        let folded = {
            let store = self.store.borrow();
            store
                .categories
                .iter()
                .find(|c| c.collapsed && c.channel_ids.contains(channel))
                .map(|c| c.id.clone())
        };
        let Some(id) = folded else { return };
        let changed = !self
            .store
            .borrow_mut()
            .set_category_collapsed(&id, false)
            .is_empty();
        if !changed {
            return;
        }
        let category = self.store.borrow().category(&id).cloned();
        if let Some(category) = category {
            self.commands.send(Command::SetCategory(category)).ok();
        }
    }

    /// Write down what is half-typed, wherever it belongs.
    ///
    /// Both boxes, because a reply left in a thread is as easy to lose
    /// as a message left in a channel -- and until now both were lost
    /// the moment the reader clicked somewhere else: the text was
    /// widget state and nothing else.
    fn remember_drafts(&self) {
        let open = self.selected.borrow().clone();
        if let Some(channel) = open {
            self.remember_draft(&channel, None, &self.composer);
        }
        let root = self.thread_root.borrow().clone();
        if let Some(root) = root {
            let channel = self
                .store
                .borrow()
                .posts
                .get(&root)
                .map(|post| post.channel_id.clone());
            if let Some(channel) = channel {
                self.remember_draft(&channel, Some(&root), &self.thread_composer);
            }
        }
    }

    /// One box's draft, recorded here and sent on.
    ///
    /// Nothing is sent when nothing changed, which is what keeps this
    /// off the network on every keystroke: the store answers with no
    /// changes when the text is the one it already holds.
    fn remember_draft(&self, channel: &ChannelId, root: Option<&PostId>, composer: &Composer) {
        let draft = mm_types::Draft {
            channel_id: channel.clone(),
            root_id: root.cloned().unwrap_or_default(),
            message: composer_text(&composer.view),
            // The Drafts view orders by this. The store has no clock on
            // purpose -- replaying events has to be exact -- so the
            // window stamps it.
            update_at: now(),
            ..mm_types::Draft::default()
        };
        if self
            .store
            .borrow_mut()
            .set_draft_locally(draft.clone())
            .is_empty()
        {
            return;
        }
        self.commands.send(Command::SaveDraft(draft)).ok();
        // Whether there is a Drafts row at all depends on this, and the
        // sections themselves have not changed.
        self.sidebar_stale.set(true);
        self.sidebar_soon();
    }

    /// Take in the names the server offered for a half-typed `@`.
    fn on_mentions_like(&self, event: Event) {
        let Event::MentionsLike {
            channel,
            prefix,
            in_channel,
            elsewhere,
        } = event
        else {
            return;
        };
        // An answer to letters already typed past is not wrong, only
        // late: the names still go in, but the popup is not redrawn
        // from them.
        let wanted = *self.mentions_asked.borrow() == prefix;
        {
            let mut store = self.store.borrow_mut();
            store.extend_channel_roster(&channel, in_channel);
            store.apply_users(elsewhere);
        }
        let refill = self.mentions_refill.borrow().clone();
        if let (true, Some(refill)) = (wanted, refill) {
            refill();
        }
    }

    /// The button that marks one followed thread read.
    ///
    /// Only on a thread that has something unread in it: a button
    /// that does nothing on two rows in three is noise, and
    /// Mattermost shows it the same way.
    fn mark_thread_read_button(&self, thread: &mm_types::UserThread) -> Option<gtk::Button> {
        if !thread.is_unread() {
            return None;
        }
        let team = self
            .store
            .borrow()
            .teams
            .first()
            .map(|team| team.id.clone())?;
        let button = gtk::Button::from_icon_name("mail-mark-read-symbolic");
        button.add_css_class("flat");
        button.set_tooltip_text(Some("Mark this thread read"));
        // Or the click reaches the row underneath and opens the very
        // thread that is being dismissed.
        button.set_can_focus(false);
        let ui = self.clone();
        let root = thread.id.clone();
        let upto = thread.last_reply_at;
        button.connect_clicked(move |_| {
            if !ui.store.borrow_mut().read_thread_locally(&root).is_empty() {
                ui.sidebar_stale.set(true);
                ui.refresh_threads_view();
                ui.sidebar_soon();
            }
            ui.commands
                .send(Command::ReadThread {
                    team: team.clone(),
                    root: root.clone(),
                    upto,
                })
                .ok();
        });
        Some(button)
    }

    /// Whose face is drawn anywhere right now.
    ///
    /// The sidebar's one-to-one conversations and the open channel's
    /// authors. Not the whole roster: a channel of two hundred would
    /// make this a two-hundred-id request every minute for dots
    /// nobody is looking at.
    fn faces_on_screen(&self) -> Vec<UserId> {
        let store = self.store.borrow();
        let mut who: Vec<UserId> = store
            .sidebar()
            .iter()
            .filter_map(|row| row.avatar_user.clone())
            .collect();
        if let Some(channel) = self.selected.borrow().clone() {
            who.extend(
                store
                    .channel_posts(&channel)
                    .iter()
                    .map(|p| p.user_id.clone()),
            );
        }
        who.sort();
        who.dedup();
        who
    }

    /// Ask the server for more names behind a half-typed `@`.
    ///
    /// Only when the store is short of them: on a small server, or
    /// for a name already seen, the local list is the whole answer and
    /// a request would be waste. And only once per prefix, because
    /// this runs on every keystroke.
    fn ask_for_mentions(&self, prefix: &str, channel: Option<&ChannelId>) {
        let Some(channel) = channel else { return };
        if prefix.chars().count() < MENTIONS_SHORTEST {
            return;
        }
        let (enough, team) = {
            let store = self.store.borrow();
            (
                store
                    .mention_candidates(Some(channel), prefix, MENTIONS_OFFERED)
                    .len()
                    >= MENTIONS_ENOUGH,
                store.teams.first().map(|team| team.id.clone()),
            )
        };
        let Some(team) = team else { return };
        if enough || *self.mentions_asked.borrow() == *prefix {
            return;
        }
        prefix.clone_into(&mut self.mentions_asked.borrow_mut());
        self.commands
            .send(Command::MentionsLike {
                team,
                channel: channel.clone(),
                prefix: prefix.to_owned(),
            })
            .ok();
    }

    /// Put a thread's half-typed reply back in its box.
    fn restore_thread_draft(&self, root: &PostId) {
        let text = {
            let store = self.store.borrow();
            store
                .posts
                .get(root)
                .and_then(|post| store.draft_for(&post.channel_id, Some(root)))
                .map_or_else(String::new, |draft| draft.message.clone())
        };
        self.fill_quietly(&self.thread_composer, &text);
    }

    /// Fill the open channel's box from a draft that has just arrived.
    ///
    /// The channel was opened before the drafts were: on a launch the
    /// reopened one is on screen within a second and the drafts come
    /// with the rest of the sync, so without this a message left here
    /// yesterday is in the Drafts view and nowhere else.
    ///
    /// Only when the box is empty. This also lands on a resync, and a
    /// reader mid-sentence must not have what they are typing replaced
    /// by what the server last heard.
    fn refill_composer(&self) {
        let open = self.selected.borrow().clone();
        if let Some(channel) = open
            && composer_text(&self.composer.view).is_empty()
        {
            self.restore_draft(&channel);
        }
    }

    /// Put a channel's half-typed message back in the box.
    fn restore_draft(&self, channel: &ChannelId) {
        let text = self
            .store
            .borrow()
            .draft_for(channel, None)
            .map_or_else(String::new, |draft| draft.message.clone());
        self.fill_quietly(&self.composer, &text);
    }

    /// Put text in a box without it counting as typing.
    fn fill_quietly(&self, composer: &Composer, text: &str) {
        self.restoring.set(true);
        composer.view.buffer().set_text(text);
        self.restoring.set(false);
    }

    /// Go to a channel: what it opens, and what it closes.
    fn open_channel(&self, id: &ChannelId) {
        // `refresh_sidebar` rebuilds every row and re-selects the open
        // channel, so the selection handler fires on its own output.
        // Without this guard each WebSocket event -- including a
        // stranger's `typing` -- re-fetched the whole channel over the
        // network, rebuilt the post list and wrote a mark-read. Only a
        // real change of channel is a user action.
        if self.selected.borrow().as_ref() == Some(id) {
            return;
        }

        // Before anything moves: what is in the boxes belongs to where
        // they were, and a channel switch is the commonest way to lose
        // it.
        self.remember_drafts();

        // Opening a channel always starts at the newest message, and
        // closes whatever thread was open: a thread belongs to the
        // channel it came from, and leaving it up beside a different one
        // shows a conversation that is not there.
        self.stick_to_bottom.set(true);
        // Before `MarkRead` below, which moves it.
        *self.unread_from.borrow_mut() = self.store.borrow().last_viewed(id);
        *self.thread_root.borrow_mut() = None;
        self.thread_posts.remove_all();
        self.thread_pane.set_visible(false);
        *self.selected.borrow_mut() = Some(id.clone());
        self.restore_draft(id);
        self.show_pane(&Pane::Channel);
        // Draw what is already held, before asking for anything. A
        // channel read earlier, or warmed in the background, is in the
        // store already -- and waiting for the round trip to redraw it
        // was most of what opening a channel felt like. The answer,
        // when it comes, splices in only what differs.
        self.refresh_posts(id);
        // The caret belongs in the composer the moment a channel opens,
        // the way Mattermost puts it there: clicking a channel is how a
        // reader starts writing in it, and the alternative is a second
        // click at the bottom of the window before the first letter.
        self.composer.view.grab_focus();
        // Written down now rather than at close, so a client killed or
        // crashed still comes back here -- but only when the reader
        // chose this. The first-channel fallback's stand-in would
        // otherwise overwrite the very channel being restored, which
        // is how "reopen where I was" quietly became "reopen whatever
        // sorts first" after one launch.
        if !self.provisional.replace(false) {
            self.remember_state();
        }
        self.commands.send(Command::OpenChannel(id.clone())).ok();
        self.commands.send(Command::MarkRead(id.clone())).ok();
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
                highlight: Vec::new(),
                action: self.mark_thread_read_button(thread),
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
                highlight: Vec::new(),
                action: None,
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
    /// A channel was dropped somewhere in the sidebar.
    ///
    /// Answers whether the drop was taken, which is what tells GTK to
    /// finish the drag rather than snap it back.
    fn drop_channel(&self, channel: &ChannelId, y: f64) -> bool {
        let Some((category, place)) = self.landing(y) else {
            return false;
        };
        let moved = self
            .store
            .borrow_mut()
            .move_channel(channel, &category, &place);
        if moved.is_empty() {
            return false;
        }
        self.commands.send(Command::SetCategories(moved)).ok();
        // The grouping changed without any channel's unread changing,
        // and the sidebar's redraw check compares sections, so say
        // outright that it has moved.
        self.sidebar_stale.set(true);
        self.sidebar_soon();
        true
    }

    /// Which category a drop at this height lands in, and where in it.
    ///
    /// On a heading: the top of that category, which is what dropping on
    /// a folded one has to mean, since a folded category shows no rows
    /// to aim between. On a channel: above that channel, in whichever
    /// category the nearest heading above it names.
    fn landing(&self, y: f64) -> Option<(String, mm_core::Place)> {
        let row = self.sidebar.row_at_y(pixels(y))?;
        if row.has_css_class("mmrs-section") {
            return Some((row.widget_name().to_string(), mm_core::Place::Front));
        }
        let name = row.widget_name();
        // Threads and Drafts are not channels and have no category.
        if name.starts_with("mmrs:") {
            return None;
        }
        let place = mm_core::Place::Before(ChannelId::new(name.as_str()));
        let mut at = row.index() - 1;
        while at >= 0 {
            let above = self.sidebar.row_at_index(at)?;
            if above.has_css_class("mmrs-section") {
                return Some((above.widget_name().to_string(), place));
            }
            at -= 1;
        }
        None
    }

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

        // Draggable into another category, which is how Mattermost lets
        // the sidebar be rearranged. The id is the whole payload: where
        // it came from is a question the store can answer at drop time,
        // and carrying it would be a second copy to go stale.
        let drag = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        let carried = row.channel.to_string();
        drag.set_content(Some(&gdk::ContentProvider::for_value(&carried.to_value())));
        // The row itself as the drag image, so what is being moved is
        // visible under the pointer rather than a generic icon.
        let dragged = line.clone();
        drag.connect_drag_begin(move |source, _| {
            let icon = gtk::WidgetPaintable::new(Some(&dragged)).current_image();
            source.set_icon(Some(&icon), 0, 0);
        });
        list_row.add_controller(drag);
        list_row
    }

    /// Where this reader is, as far as the store knows.
    fn my_presence(&self) -> mm_types::Presence {
        let store = self.store.borrow();
        store.presence(&store.me)
    }

    /// Redraw the dot beside the account name.
    fn refresh_presence(&self) {
        self.presence
            .set_child(Some(&presence_dot(self.my_presence())));
    }

    fn refresh_sidebar(&self) {
        self.refresh_badge();
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
            // the reader is in.
            let showing: Vec<&mm_core::SidebarRow> = section
                .visible_rows(keep.as_ref())
                .into_iter()
                .filter(|row| {
                    !self.sidebar_unread_only.get()
                        || row.is_unread()
                        || keep.as_ref() == Some(&row.channel)
                })
                .collect();
            // Two different kinds of nothing, and only one of them is a
            // reason to leave the heading out.
            //
            // A category with no channels in it is not drawn: a bare
            // heading over nothing says nothing. The unread filter is
            // the reader asking to see only what wants attention, so a
            // section with nothing left under it goes with its heading.
            //
            // But a category the reader has **folded shut** is full; it
            // is showing nothing because that is what folding means.
            // Dropping its heading took away the one thing that could
            // unfold it, so Team Backend and DevOps -- collapsed on the
            // server, with nothing unread in them -- were simply absent
            // from the sidebar with no way to get them back.
            if section.rows.is_empty() || (showing.is_empty() && self.sidebar_unread_only.get()) {
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
        // Back to where the reader was, as soon as that channel is in
        // the sidebar. Tried on every redraw until it lands, because
        // the first few draws happen before the channel list has
        // arrived -- consuming it on the first attempt is how this
        // silently fell back to whatever sorts first.
        // A link the reader followed wins over where they were last
        // time: they asked for this one just now.
        if self.follow_pending_link() || self.restore_reopen() {
            return;
        }

        match to_select {
            Some(row) => self.sidebar.select_row(Some(&row)),
            // Nothing known yet: open the first channel rather than
            // leaving the message pane blank.
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
                    // Before the select, because the handler reads it:
                    // what the fallback chose is a stand-in for not
                    // knowing yet, and must not be written down over
                    // the channel still being restored.
                    self.provisional.set(self.reopen.borrow().is_some());
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
        let mut blocks = render_blocks(&doc, &self.emoji);
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
                    inlines: Vec::new(),
                    emoji: Vec::new(),
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
            author_id: post.user_id.clone(),
            body: "",
            blocks,
            time: &clock_time(post.create_at.millis()),
            ago: &describe_ago(post.create_at),
            full_time: &full_time(post.create_at.millis()),
            files: post
                .attachments()
                .into_iter()
                // Anything drawn as a picture above names itself.
                .filter(|f| {
                    !(f.is_image() && f.fitted(IMAGE_MAX_WIDTH, IMAGE_MAX_HEIGHT).is_some())
                })
                .collect(),
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
                .map(|a| render_card(a, store, &self.emoji))
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
        // The toggle says what the server says, and setting it must not
        // read as a click -- which is what the guard is for.
        self.setting_follow.set(true);
        self.thread_follow.set_active(store.is_following(&root));
        self.setting_follow.set(false);
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
                // The reply half-typed in whatever thread was open
                // before this one, before the root that names it moves.
                self.remember_drafts();
                *self.thread_root.borrow_mut() = Some(root.clone());
                self.restore_thread_draft(&root);
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
                self.refill_composer();
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
            Event::SearchResults { .. } | Event::PinnedPosts { .. } | Event::SavedPosts(_) => {
                self.on_found(event);
            }
            Event::Elsewhere { .. } | Event::Joined { .. } | Event::Opened(_) => {
                self.on_elsewhere(event);
            }
            Event::Statuses(statuses) => {
                if !self.store.borrow_mut().apply_statuses(&statuses).is_empty() {
                    self.refresh_presence();
                    self.redraw_soon();
                }
            }
            Event::CustomEmoji(_) | Event::EmojiImage { .. } => self.on_emoji(event),
            Event::MentionsLike { .. } => self.on_mentions_like(event),
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

    /// Underline every word in a composer that no dictionary knows.
    ///
    /// The whole buffer each time rather than the word just typed: a
    /// message is a few hundred characters, checking it is microseconds,
    /// and tracking which span changed is how a stale underline survives
    /// an edit three words to its left.
    fn mark_mistakes(&self, buffer: &gtk::TextBuffer) {
        let spelling = self.spelling.borrow();
        let Some(spelling) = spelling.as_ref() else {
            return;
        };
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name(MISSPELLED, &start, &end);
        let text = buffer.text(&start, &end, false).to_string();
        for word in spelling.mistakes(&text) {
            buffer.apply_tag_by_name(
                MISSPELLED,
                &buffer.iter_at_offset(offset(word.start)),
                &buffer.iter_at_offset(offset(word.end)),
            );
        }
    }

    /// The dictionaries have finished loading on their own thread.
    fn use_spelling(&self, spelling: crate::spelling::Spelling) {
        let languages = spelling.languages().join(", ");
        *self.spelling.borrow_mut() = Some(spelling);
        if languages.is_empty() {
            // Said once, quietly: no dictionaries installed is a thing
            // the reader can fix, and silent absence is not a clue.
            self.set_status("no spelling dictionaries found");
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
            Pane::Found(which) => self.refresh_found(which),
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
            // Cheap, and the alternative is a second path for the one
            // case that moves it from outside: a `status_change` for
            // this reader, sent when they set it in another client.
            ui.refresh_presence();
            ui.redraw_everything();
        });
    }

    /// The server's own emoji: the list, and the pictures.
    fn on_emoji(&self, event: Event) {
        match event {
            // A redraw, because the list arrives *after* the messages
            // holding it: every `:party_parrot:` already on screen was
            // drawn as text, and nothing else would ever ask again.
            // Coalesced, and only when the list actually moved, so a
            // resync that learns nothing costs nothing.
            Event::CustomEmoji(emoji) => {
                let moved = !self
                    .store
                    .borrow_mut()
                    .apply_custom_emoji(&emoji)
                    .is_empty();
                if moved {
                    self.redraw_soon();
                }
            }
            // Coalesced, and it has to be: the emoji on a busy
            // channel's reactions all land within a few milliseconds of
            // each other, and each one would otherwise redraw the lot.
            Event::EmojiImage { name, image } => {
                if self.emoji.received(name, image) {
                    self.redraw_soon();
                    self.emoji.nudge_picker();
                }
            }
            _ => {}
        }
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
            set_presence(&dot, self.store.borrow().presence(user), true);
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
        self.composer_pane.set_sensitive(true);

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
