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
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct PostRow {
        pub id: RefCell<String>,
        pub author: RefCell<String>,
        pub body: RefCell<String>,
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

impl PostRow {
    #[must_use]
    pub fn new(id: &str, author: &str, body: &str) -> Self {
        let this: Self = glib::Object::new();
        let inner = this.imp();
        inner.id.replace(id.to_owned());
        inner.author.replace(author.to_owned());
        inner.body.replace(body.to_owned());
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
}
