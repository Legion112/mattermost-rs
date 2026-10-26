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
use crate::row::{PostRow, PostRowFields};
// adw::prelude re-exports gtk::prelude, so this one import covers both.
use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use mm_core::{PageAnchor, Store, Threading};
use mm_types::{ChannelId, UserId};
use std::cell::RefCell;
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
            pump.status.set_text("the engine stopped");
        });
    });

    app.run_with_args::<&str>(&[])
}

fn build(app: &adw::Application, commands: &tokio::sync::mpsc::UnboundedSender<Command>) -> Ui {
    let sidebar = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .child(&sidebar)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(260)
        .build();

    let (posts, post_view, scroller) = build_post_list();

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

    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar_scroll)
        .end_child(&content)
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

/// Connect the widgets to the command channel.
///
/// Separate from `build` only so neither function grows past the point where
/// it can be read in one go.
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

        // Arriving back at the bottom opts back in.
        let stick = Rc::clone(&ui.stick_to_bottom);
        ui.scroller.connect_edge_reached(move |_, edge| {
            if edge == gtk::PositionType::Bottom {
                stick.set(true);
            }
        });
    }
    {
        let commands = commands.clone();
        let selected = Rc::clone(&ui.selected);
        let stick = Rc::clone(&ui.stick_to_bottom);
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

            // Opening a channel always starts at the newest message.
            stick.set(true);
            *selected.borrow_mut() = Some(id.clone());
            commands.send(Command::OpenChannel(id.clone())).ok();
            commands.send(Command::MarkRead(id)).ok();
        });
    }
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
            commands.send(Command::Send { channel, text }).ok();
            entry.set_text("");
        });
    }
}

/// Render a parsed message to Pango markup.
///
/// Block structure is flattened to lines: block-level widgets per paragraph
/// arrive with the component tree, and emitting them as separate labels now
/// would mean rebuilding the row factory for a layout that is about to change.
fn render_markup(doc: &mm_md::Document) -> String {
    let mut out = String::new();
    for block in &doc.blocks {
        if !out.is_empty() {
            out.push('\n');
        }
        match block {
            mm_md::Block::Paragraph(inlines) | mm_md::Block::Heading { inlines, .. } => {
                out.push_str(&mm_md::to_pango(inlines));
            }
            mm_md::Block::ListItem(inlines) => {
                out.push_str("• ");
                out.push_str(&mm_md::to_pango(inlines));
            }
            mm_md::Block::Code { code, .. } => {
                out.push_str("<tt>");
                out.push_str(&mm_md::escape(code.trim_end()));
                out.push_str("</tt>");
            }
            mm_md::Block::Quote(blocks) => {
                let inner = mm_md::Document {
                    blocks: blocks.clone(),
                };
                out.push_str("<i>");
                out.push_str(&render_markup(&inner));
                out.push_str("</i>");
            }
            mm_md::Block::Rule => out.push_str("───"),
        }
    }
    out
}

/// The trailing line: what is attached, and how many replies there are.
///
/// Without this a post whose only content is a file renders as an author, a
/// timestamp and nothing at all, which reads as the client being broken.
fn describe_extras(post: &mm_types::Post, replies: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    for file in post.attachments() {
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
    if replies > 0 {
        parts.push(format!(
            "💬 {replies} {}",
            if replies == 1 { "reply" } else { "replies" }
        ));
    }
    parts.join("   ")
}

/// `HH:MM` in local time.
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
fn build_post_list() -> (gio::ListStore, gtk::ListView, gtk::ScrolledWindow) {
    let posts = gio::ListStore::new::<PostRow>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        row.set_margin_start(12);
        row.set_margin_end(12);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let author = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let time = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        header.append(&author);
        header.append(&time);
        // A non-editable TextView rather than a Label: selection across the
        // whole message, and the inline paintables custom emoji will need.
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        // Attachments and "N replies" -- the line that keeps a post whose
        // only content is a file from rendering as an author and nothing.
        let meta = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        row.append(&header);
        row.append(&body);
        row.append(&meta);
        item.downcast_ref::<gtk::ListItem>()
            .expect("a list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let Some(data) = item.item().and_downcast::<PostRow>() else {
            return;
        };
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        // Walk the children by position rather than first/last: there are
        // three now, and last_child() silently became the wrong one when the
        // meta line was added.
        let Some(header) = row.first_child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(body) = header.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(meta) = body.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };

        let system = data.is_system();
        if let Some(author) = header.first_child().and_downcast::<gtk::Label>() {
            // A join/leave notice is not somebody speaking, so it gets no
            // author line.
            let name = data.author();
            author.set_text(if system { "" } else { name.as_str() });
        }
        if let Some(time) = header.last_child().and_downcast::<gtk::Label>() {
            time.set_text(&data.time());
        }

        // set_markup on text that is not markup would make GTK log an error
        // and drop the message, so the flag decides.
        if data.is_markup() {
            body.set_markup(&data.body());
        } else {
            body.set_text(&data.body());
        }
        if system {
            body.add_css_class("dim-label");
        } else {
            body.remove_css_class("dim-label");
        }
        body.set_visible(!data.body().is_empty());

        let extra = data.meta();
        meta.set_text(&extra);
        meta.set_visible(!extra.is_empty());
    });

    let post_view = gtk::ListView::builder()
        .model(&gtk::NoSelection::new(Some(posts.clone())))
        .factory(&factory)
        .single_click_activate(false)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&post_view)
        .vexpand(true)
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
                self.title.set_subtitle("not signed in");
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
                self.title.set_subtitle(&format!("{username} · {server}"));
                self.status.set_text("connected");
            }
            Event::Threading(threading) => {
                self.store.borrow_mut().threading = threading;
            }
            Event::Teams(teams) => self.store.borrow_mut().apply_teams(teams),
            Event::Channels(channels) => {
                self.store.borrow_mut().apply_channels(channels);
                self.refresh_sidebar();
            }
            Event::Members(members) => {
                self.store.borrow_mut().apply_members(members);
                self.refresh_sidebar();
            }
            Event::Users(users) => {
                self.store.borrow_mut().apply_users(users);
                self.refresh_sidebar();
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
            }
            Event::ResyncRequired(why) => {
                self.status.set_text(&format!("reconnected: {why}"));
                self.commands.send(Command::Sync).ok();
            }
            Event::Status(text) => self.status.set_text(&text),
            Event::Failed(text) => self.status.set_text(&format!("error: {text}")),
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
            store
                .users
                .get(&post.user_id)
                .map_or_else(|| post.user_id.to_string(), |u| u.display_name().to_owned())
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
        let rows = self.store.borrow().sidebar();
        let keep = self.selected.borrow().clone();

        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }

        let mut to_select = None;
        for row in &rows {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
        match to_select {
            Some(row) => self.sidebar.select_row(Some(&row)),
            // Nothing selected yet: open the first channel rather than
            // leaving the message pane blank. The sidebar is sorted unread
            // first, so this lands on whatever most wants attention.
            None if keep.is_none() => {
                if let Some(first) = self.sidebar.row_at_index(0) {
                    self.sidebar.select_row(Some(&first));
                }
            }
            None => {}
        }
    }

    fn refresh_posts(&self, channel: &ChannelId) {
        // Was the newest message in view before this update? Asked of the
        // model, not of the scroll adjustment: `upper` is an estimate GTK
        // revises as rows are realized, and it was observed moving 20% in both
        // directions inside one run. See docs/gtk-notes.md.
        let was_at_end = self.posts.n_items() == 0 || self.stick_to_bottom.get();

        let store = self.store.borrow();
        self.posts.remove_all();
        for post in store.channel_posts(channel) {
            let author = store
                .users
                .get(&post.user_id)
                .map_or_else(|| post.user_id.to_string(), |u| u.display_name().to_owned());
            let suffix = if post.is_edited() { " (edited)" } else { "" };
            let doc = mm_md::parse(&post.message);
            // Most messages are a single plain paragraph, and for those the
            // markup round-trip is skipped entirely.
            let (body, markup) = if let Some(plain) = doc.plain_text() {
                (format!("{plain}{suffix}"), false)
            } else {
                let rendered = render_markup(&doc);
                (format!("{rendered}{}", mm_md::escape(suffix)), true)
            };
            self.posts.append(&PostRow::new(PostRowFields {
                id: post.id.as_str(),
                author: &author,
                body: &body,
                time: &clock_time(post.create_at.millis()),
                meta: &describe_extras(post, store.reply_count(&post.id)),
                markup,
                system: post.is_system(),
            }));
        }
        self.composer.set_sensitive(true);

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
