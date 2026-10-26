//! The post-list item.
//!
//! A `GObject` carrying **only strings already derived from the store**, never
//! a widget and never a `Post`. The list model holds one of these per visible
//! row's worth of data; `gtk::ListView` recycles a small number of actual
//! widgets across them, which is what makes a hundred-thousand-post channel
//! open at all.

use gtk::glib;

/// One image attached to a message, at the size it will be drawn.
///
/// The texture is optional: a file whose thumbnail could not be decoded and
/// whose full image has not arrived yet still reserves its space, so the
/// list does not jump when it lands.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub texture: Option<gtk::gdk::Texture>,
    pub width: i32,
    pub height: i32,
}
use gtk::subclass::prelude::*;

mod imp {
    use super::{ObjectImpl, ObjectSubclass, glib};
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Default)]
    pub struct PostRow {
        pub id: RefCell<String>,
        pub author: RefCell<String>,
        pub body: RefCell<String>,
        pub time: RefCell<String>,
        /// A trailing line for attachments and reply counts; empty hides it.
        pub meta: RefCell<String>,
        /// A day boundary rather than a message. `body` carries the label.
        pub separator: Cell<bool>,
        /// A separator that marks where the reader left off, drawn in the
        /// accent colour rather than as a plain rule.
        pub unread_mark: Cell<bool>,
        /// The same author is still talking, so the author line is redundant
        /// and this row draws tight under the one above it.
        pub grouped: Cell<bool>,
        /// Join/leave notices and the like, which Mattermost renders muted
        /// and without an author rather than as somebody speaking.
        pub system: Cell<bool>,
        /// Whether `body` is Pango markup rather than literal text. Most
        /// messages are plain, and for those the markup round-trip is skipped
        /// entirely -- that fast path is where the rendering cost goes in a
        /// busy channel.
        pub markup: Cell<bool>,
        /// Already grouped and ordered by `mm-core`; the row only draws them.
        pub reactions: RefCell<Vec<mm_core::ReactionSummary>>,
        /// The author's face, once it has been fetched and decoded. `None`
        /// draws initials instead, so a row never waits on the network.
        pub avatar: RefCell<Option<gtk::gdk::Texture>>,
        /// How many replies hang off this post, for the button that opens
        /// the thread. Zero on a reply, which has no thread of its own.
        pub replies: Cell<i64>,
        /// Images attached to the message, already sized.
        pub images: RefCell<Vec<super::Attachment>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PostRow {
        const NAME: &'static str = "MmrsPostRow";
        type Type = super::PostRow;
    }

    impl ObjectImpl for PostRow {}
}

glib::wrapper! {
    pub struct PostRow(ObjectSubclass<imp::PostRow>);
}

/// Grouped so adding a field does not mean editing every call site, and so
/// the call site reads as names rather than five positional strings.
#[derive(Debug, Clone)]
pub struct PostRowFields<'a> {
    pub id: &'a str,
    pub author: &'a str,
    pub body: &'a str,
    pub time: &'a str,
    pub meta: &'a str,
    pub markup: bool,
    pub system: bool,
    pub grouped: bool,
    pub reactions: Vec<mm_core::ReactionSummary>,
    pub avatar: Option<gtk::gdk::Texture>,
    pub replies: i64,
    pub images: Vec<Attachment>,
}

impl PostRow {
    #[must_use]
    pub fn new(fields: PostRowFields<'_>) -> Self {
        let this: Self = glib::Object::new();
        let inner = this.imp();
        inner.id.replace(fields.id.to_owned());
        inner.author.replace(fields.author.to_owned());
        inner.body.replace(fields.body.to_owned());
        inner.time.replace(fields.time.to_owned());
        inner.meta.replace(fields.meta.to_owned());
        inner.markup.set(fields.markup);
        inner.system.set(fields.system);
        inner.grouped.set(fields.grouped);
        inner.reactions.replace(fields.reactions);
        inner.avatar.replace(fields.avatar);
        inner.replies.set(fields.replies);
        inner.images.replace(fields.images);
        this
    }

    /// A day boundary. Carries its label in `body` because that is the one
    /// string every row kind has, and a separator has nothing else.
    #[must_use]
    pub fn separator(label: &str) -> Self {
        let this: Self = glib::Object::new();
        this.imp().body.replace(label.to_owned());
        this.imp().separator.set(true);
        this
    }

    /// The "new messages" line: a separator that draws attention.
    #[must_use]
    pub fn new_messages() -> Self {
        let this = Self::separator("New messages");
        this.imp().unread_mark.set(true);
        this
    }

    #[must_use]
    pub fn is_unread_mark(&self) -> bool {
        self.imp().unread_mark.get()
    }

    #[must_use]
    pub fn is_separator(&self) -> bool {
        self.imp().separator.get()
    }

    #[must_use]
    pub fn images(&self) -> Vec<Attachment> {
        self.imp().images.borrow().clone()
    }

    #[must_use]
    pub fn replies(&self) -> i64 {
        self.imp().replies.get()
    }

    #[must_use]
    pub fn avatar(&self) -> Option<gtk::gdk::Texture> {
        self.imp().avatar.borrow().clone()
    }

    #[must_use]
    pub fn reactions(&self) -> Vec<mm_core::ReactionSummary> {
        self.imp().reactions.borrow().clone()
    }

    #[must_use]
    pub fn is_grouped(&self) -> bool {
        self.imp().grouped.get()
    }

    #[must_use]
    pub fn id(&self) -> String {
        self.imp().id.borrow().clone()
    }

    #[must_use]
    pub fn author(&self) -> String {
        self.imp().author.borrow().clone()
    }

    #[must_use]
    pub fn body(&self) -> String {
        self.imp().body.borrow().clone()
    }

    #[must_use]
    pub fn time(&self) -> String {
        self.imp().time.borrow().clone()
    }

    #[must_use]
    pub fn is_markup(&self) -> bool {
        self.imp().markup.get()
    }

    #[must_use]
    pub fn meta(&self) -> String {
        self.imp().meta.borrow().clone()
    }

    #[must_use]
    pub fn is_system(&self) -> bool {
        self.imp().system.get()
    }
}
