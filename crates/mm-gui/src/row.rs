//! The post-list item.
//!
//! A `GObject` carrying **only strings already derived from the store**, never
//! a widget and never a `Post`. The list model holds one of these per visible
//! row's worth of data; `gtk::ListView` recycles a small number of actual
//! widgets across them, which is what makes a hundred-thousand-post channel
//! open at all.

use gtk::glib;
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
        /// Join/leave notices and the like, which Mattermost renders muted
        /// and without an author rather than as somebody speaking.
        pub system: Cell<bool>,
        /// Whether `body` is Pango markup rather than literal text. Most
        /// messages are plain, and for those the markup round-trip is skipped
        /// entirely -- that fast path is where the rendering cost goes in a
        /// busy channel.
        pub markup: Cell<bool>,
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
#[derive(Debug, Clone, Copy)]
pub struct PostRowFields<'a> {
    pub id: &'a str,
    pub author: &'a str,
    pub body: &'a str,
    pub time: &'a str,
    pub meta: &'a str,
    pub markup: bool,
    pub system: bool,
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
        this
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
