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
use crate::row::PostRow;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use mm_core::{PageAnchor, Store, Threading};
use mm_types::{ChannelId, UserId};
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "dev.legion.mattermost_rs";

/// Everything the window needs to talk to the outside world.
#[derive(Debug)]
pub struct Wiring {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub events: async_channel::Receiver<Event>,
}

struct Ui {
    store: Rc<RefCell<Store>>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    selected: Rc<RefCell<Option<ChannelId>>>,
    sidebar: gtk::ListBox,
    posts: gio::ListStore,
    post_view: gtk::ListView,
    scroller: gtk::ScrolledWindow,
    status: gtk::Label,
    title: adw::WindowTitle,
    composer: gtk::Entry,
}

/// Build the window and run the GTK main loop.
#[must_use]
pub fn run(wiring: Wiring) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
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

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&split);

    let window = adw::ApplicationWindow::builder()
        .application(app)
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
        status,
        title,
        composer: composer.clone(),
    };

    wire(&ui, &refresh, &sidebar, &composer, commands);
    ui
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
        let commands = commands.clone();
        let selected = Rc::clone(&ui.selected);
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let id = ChannelId::new(row.widget_name().to_string());
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
        let author = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        // A non-editable TextView rather than a Label: selection across the
        // whole message, and the inline paintables custom emoji will need.
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        row.append(&author);
        row.append(&body);
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
        if let Some(author) = row.first_child().and_downcast::<gtk::Label>() {
            author.set_text(&data.author());
        }
        if let Some(body) = row.last_child().and_downcast::<gtk::Label>() {
            body.set_text(&data.body());
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
            Event::Connected {
                server,
                me,
                username,
            } => {
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
        let was_at_end = {
            let n = self.posts.n_items();
            n == 0 || {
                let adj = self.scroller.vadjustment();
                adj.value() + adj.page_size() >= adj.upper() - 1.0
            }
        };

        let store = self.store.borrow();
        self.posts.remove_all();
        for post in store.channel_posts(channel) {
            let author = store
                .users
                .get(&post.user_id)
                .map_or_else(|| post.user_id.to_string(), |u| u.display_name().to_owned());
            let suffix = if post.is_edited() { " (edited)" } else { "" };
            self.posts.append(&PostRow::new(
                post.id.as_str(),
                &author,
                &format!("{}{suffix}", post.message),
            ));
        }
        self.composer.set_sensitive(true);

        if was_at_end {
            let n = self.posts.n_items();
            if n > 0 {
                // scroll_to is the widget's own API. Adjustment arithmetic is
                // not reliable here, for the reason above.
                self.post_view
                    .scroll_to(n - 1, gtk::ListScrollFlags::empty(), None);
            }
        }
    }
}
