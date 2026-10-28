//! The post-list item.
//!
//! A `GObject` carrying **only strings already derived from the store**, never
//! a widget and never a `Post`. The list model holds one of these per visible
//! row's worth of data; `gtk::ListView` recycles a small number of actual
//! widgets across them, which is what makes a hundred-thousand-post channel
//! open at all.

use gtk::glib;

/// What kind of widget one block of a message becomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlockKind {
    /// A paragraph or a heading.
    #[default]
    Text,
    /// A list item, which gets a bullet and an indent.
    Bullet,
    /// A fenced or indented code block, which gets a box of its own.
    Code,
    /// A horizontal rule.
    Rule,
    /// A table, drawn as a grid. `text` is unused; `cells` carries it.
    Table,
}

/// One block of a message, already rendered to what the row draws.
///
/// A message is a sequence of these rather than one string of markup,
/// because a code block is a *box* and a rule is a line: neither is
/// expressible as Pango markup, and flattening them into one label is why
/// code in this client read as ordinary monospace text while every other
/// client drew it in a panel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockView {
    pub kind: BlockKind,
    /// Pango markup, or literal text when `markup` is false.
    pub text: String,
    pub markup: bool,
    /// The same content before it was flattened to markup, for the one
    /// thing markup cannot express: a picture in the middle of a line.
    /// Empty for the blocks that have no inlines -- code, rules, and
    /// the plain-text fast path.
    pub inlines: Vec<mm_md::Inline>,
    /// The picture behind each custom-emoji shortcode in `inlines`, for
    /// the ones this client has fetched.
    ///
    /// **Resolved when the row is built, not when it is drawn.** The
    /// list replaces only the rows that differ from what is on screen,
    /// and it compares rows by their contents -- so a picture that
    /// landed after the message was drawn has to be *part of the row*
    /// or nothing will ever redraw it.
    pub emoji: Vec<(String, gtk::gdk::Texture)>,
    /// Inside a block quote, which draws a bar down the left.
    pub quoted: bool,
    /// For `Table`: the rows, each a list of cells of Pango markup, the
    /// header first.
    pub cells: Vec<Vec<String>>,
}

/// One Slack-style attachment, already rendered to what the row draws.
///
/// The markup is produced once here rather than in the bind callback,
/// because bind runs on every scroll and parsing markdown there would make
/// a channel of alerts crawl.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentView {
    /// The bar down the left, when the attachment states a colour.
    pub rgb: Option<(u8, u8, u8)>,
    pub pretext: String,
    pub title: String,
    /// Where the title points, if anywhere.
    pub title_link: String,
    /// The body, a block at a time.
    pub blocks: Vec<BlockView>,
    pub fields: Vec<(String, String)>,
    pub footer: String,
}

/// One image attached to a message, at the size it will be drawn.
///
/// The texture is optional: a file whose thumbnail could not be decoded and
/// whose full image has not arrived yet still reserves its space, so the
/// list does not jump when it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Which file this is, so a click can ask for the full-size copy.
    pub file: mm_types::FileId,
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
        pub author_id: RefCell<mm_types::UserId>,
        pub body: RefCell<String>,
        /// The message body, a block at a time. Empty for a separator,
        /// whose text lives in `body`.
        pub blocks: RefCell<Vec<super::BlockView>>,
        pub time: RefCell<String>,
        /// When it was said, for the views that say it as a distance.
        ///
        /// The moment, not the sentence. "4 minutes ago" is a string
        /// that changes every minute on its own, and a row that carries
        /// it is a row that differs from its own redraw: sixty of
        /// sixty-one rows were replaced by a clock tick, which on a real
        /// account is most of a second of rebuilding a channel nobody
        /// touched. The sentence is now written at bind time instead.
        pub at: Cell<mm_types::Timestamp>,
        pub full_time: RefCell<String>,
        /// A trailing line for attachments and reply counts; empty hides it.
        pub files: RefCell<Vec<mm_types::FileRef>>,
        /// A day boundary rather than a message. `body` carries the label.
        pub separator: Cell<bool>,
        /// A separator that marks where the reader left off, drawn in the
        /// accent colour rather than as a plain rule.
        pub unread_mark: Cell<bool>,
        /// The same author is still talking, so the author line is redundant
        /// and this row draws tight under the one above it.
        pub grouped: Cell<bool>,
        /// Who the row is from.
        pub voice: Cell<super::Voice>,
        /// Already grouped and ordered by `mm-core`; the row only draws them.
        pub reactions: RefCell<Vec<mm_core::ReactionSummary>>,
        /// The author's face, once it has been fetched and decoded. `None`
        /// draws initials instead, so a row never waits on the network.
        pub avatar: RefCell<Option<gtk::gdk::Texture>>,
        /// How many replies hang off this post, for the button that opens
        /// the thread. Zero on a reply, which has no thread of its own.
        pub replies: Cell<i64>,
        /// The message as typed, for "copy text".
        pub plain_body: RefCell<String>,
        /// The one-click reactions to offer on this row.
        pub quick_reactions: RefCell<Vec<String>>,
        /// Images attached to the message, already sized.
        pub images: RefCell<Vec<super::Attachment>>,
        /// Webhook attachments, already rendered.
        pub cards: RefCell<Vec<super::AttachmentView>>,
        pub previews: RefCell<Vec<mm_types::LinkPreview>>,
        /// Which build of this row it is.
        ///
        /// A `PostRow` is never changed after it is made: a redraw makes
        /// a new one, and `splice_changed` keeps the old object wherever
        /// nothing moved. So two rows carrying the same serial are the
        /// same object, and a widget that already shows that serial
        /// already shows that row -- which is how `bind_post_row` knows
        /// it has nothing to do.
        pub serial: Cell<u64>,
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
    /// Who said it, so a click on the name can open their card.
    pub author_id: mm_types::UserId,
    pub body: &'a str,
    pub blocks: Vec<BlockView>,
    pub time: &'a str,
    /// When it was said. The thread pane says it as a distance -- "2
    /// weeks ago" -- because it has no day separators to put a bare
    /// clock time in any context, and works that out at bind time.
    pub at: mm_types::Timestamp,
    /// The whole date and time, for the tooltip on the clock. A message
    /// says "17:01" and the reader needs to know which day that was.
    pub full_time: &'a str,
    /// Everything attached that is not drawn as a picture.
    pub files: Vec<mm_types::FileRef>,
    pub flags: PostRowFlags,
    pub reactions: Vec<mm_core::ReactionSummary>,
    pub avatar: Option<gtk::gdk::Texture>,
    pub replies: i64,
    pub images: Vec<Attachment>,
    pub cards: Vec<AttachmentView>,
    /// What the server found behind the links in the message.
    pub previews: Vec<mm_types::LinkPreview>,
    pub quick_reactions: Vec<String>,
    pub plain_body: String,
}

/// Who a row is from.
///
/// One value rather than a `system` and a `bot` flag beside each other:
/// they were never independent — a join notice is not from a bot and a bot's
/// alert is not a system message — and a pair of booleans let both be true
/// at once, which has no meaning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Voice {
    /// Somebody speaking.
    #[default]
    Person,
    /// A webhook or a bot account.
    Bot,
    /// A join/leave notice and the like, which Mattermost renders muted and
    /// without an author.
    System,
}

/// The rest of the yes-or-no facts about a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostRowFlags {
    /// The same author is still talking.
    pub grouped: bool,
    pub voice: Voice,
}

impl PostRow {
    #[must_use]
    pub fn new(fields: PostRowFields<'_>) -> Self {
        let this: Self = glib::Object::new();
        let inner = this.imp();
        inner.serial.set(next_serial());
        inner.id.replace(fields.id.to_owned());
        inner.author.replace(fields.author.to_owned());
        inner.author_id.replace(fields.author_id);
        inner.body.replace(fields.body.to_owned());
        inner.blocks.replace(fields.blocks);
        inner.time.replace(fields.time.to_owned());
        inner.at.set(fields.at);
        inner.full_time.replace(fields.full_time.to_owned());
        inner.files.replace(fields.files);
        inner.voice.set(fields.flags.voice);
        inner.grouped.set(fields.flags.grouped);
        inner.reactions.replace(fields.reactions);
        inner.avatar.replace(fields.avatar);
        inner.replies.set(fields.replies);
        inner.images.replace(fields.images);
        inner.quick_reactions.replace(fields.quick_reactions);
        inner.plain_body.replace(fields.plain_body);
        inner.cards.replace(fields.cards);
        inner.previews.replace(fields.previews);
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

    /// Whether this row would draw exactly as `other` does.
    ///
    /// What makes a redraw cheap. `refresh_posts` rebuilds every row's
    /// worth of data -- which is free, under a millisecond for a channel
    /// -- and then hands the list only the run that actually differs.
    /// Handing it the lot instead made `ListView` build a widget set for
    /// every row of the channel, every time an avatar or an image landed,
    /// and a channel grew more expensive to redraw with every page of
    /// history loaded into it.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.differs(other).is_none()
    }

    /// Which field makes these two rows different, if any.
    ///
    /// `matches` is the question the splice asks; this is the one asked
    /// when the answer is surprising. A redraw that replaces sixty rows
    /// nobody changed is a field that moves on its own, and the only way
    /// to know which is to name it.
    #[must_use]
    pub fn differs(&self, other: &Self) -> Option<&'static str> {
        let (a, b) = (self.imp(), other.imp());
        let same = [
            ("separator", a.separator.get() == b.separator.get()),
            ("unread_mark", a.unread_mark.get() == b.unread_mark.get()),
            ("voice", a.voice.get() == b.voice.get()),
            ("grouped", a.grouped.get() == b.grouped.get()),
            ("replies", a.replies.get() == b.replies.get()),
            ("id", *a.id.borrow() == *b.id.borrow()),
            ("author", *a.author.borrow() == *b.author.borrow()),
            ("body", *a.body.borrow() == *b.body.borrow()),
            ("time", *a.time.borrow() == *b.time.borrow()),
            ("files", *a.files.borrow() == *b.files.borrow()),
            ("blocks", *a.blocks.borrow() == *b.blocks.borrow()),
            ("cards", *a.cards.borrow() == *b.cards.borrow()),
            ("previews", *a.previews.borrow() == *b.previews.borrow()),
            ("images", *a.images.borrow() == *b.images.borrow()),
            ("reactions", *a.reactions.borrow() == *b.reactions.borrow()),
            (
                "quick_reactions",
                *a.quick_reactions.borrow() == *b.quick_reactions.borrow(),
            ),
            ("avatar", *a.avatar.borrow() == *b.avatar.borrow()),
        ];
        same.into_iter()
            .find_map(|(what, same)| (!same).then_some(what))
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
    pub fn cards(&self) -> Vec<AttachmentView> {
        self.imp().cards.borrow().clone()
    }

    #[must_use]
    pub fn previews(&self) -> Vec<mm_types::LinkPreview> {
        self.imp().previews.borrow().clone()
    }

    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.imp().voice.get() == Voice::Bot
    }

    #[must_use]
    pub fn plain_body(&self) -> String {
        self.imp().plain_body.borrow().clone()
    }

    #[must_use]
    pub fn quick_reactions(&self) -> Vec<String> {
        self.imp().quick_reactions.borrow().clone()
    }

    #[must_use]
    pub fn images(&self) -> Vec<Attachment> {
        self.imp().images.borrow().clone()
    }

    #[must_use]
    pub fn replies(&self) -> i64 {
        self.imp().replies.get()
    }

    /// Which build of this row it is. See the field's own note.
    #[must_use]
    pub fn serial(&self) -> u64 {
        self.imp().serial.get()
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
    pub fn author_id(&self) -> mm_types::UserId {
        self.imp().author_id.borrow().clone()
    }

    #[must_use]
    pub fn author(&self) -> String {
        self.imp().author.borrow().clone()
    }

    #[must_use]
    pub fn blocks(&self) -> Vec<BlockView> {
        self.imp().blocks.borrow().clone()
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
    pub fn at(&self) -> mm_types::Timestamp {
        self.imp().at.get()
    }

    #[must_use]
    pub fn full_time(&self) -> String {
        self.imp().full_time.borrow().clone()
    }

    #[must_use]
    pub fn files(&self) -> Vec<mm_types::FileRef> {
        self.imp().files.borrow().clone()
    }

    #[must_use]
    pub fn is_system(&self) -> bool {
        self.imp().voice.get() == Voice::System
    }
}

thread_local! {
    /// Rows are only ever made on the GUI thread, so a thread-local
    /// counter is the whole of it. Wrapping would need a channel open
    /// for ten million years at sixty redraws a second.
    static SERIAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn next_serial() -> u64 {
    SERIAL.with(|n| {
        let next = n.get().wrapping_add(1);
        n.set(next);
        next
    })
}
