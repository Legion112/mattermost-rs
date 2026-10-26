//! The client state machine: store, timeline, unread computation and
//! notification policy.
//!
//! **Inside the coverage gate, at 100%.** Nothing here performs I/O and
//! nothing here reads a clock — timestamps arrive on the data. That is what
//! lets a recorded trace replay byte-for-byte, and it is why this crate needs
//! no clock fake at all.
//!
//! The rule that matters most: **every mutation is a monotonic join, never a
//! blind assignment.** The server replays its dead queue after a reconnect, so
//! the same event genuinely arrives twice; a read-state push from another
//! device can be older than what is held; an edit can arrive out of order with
//! the page that already carried it. Under those conditions "take the newer
//! value" is wrong and "take whatever arrived last" is worse.

pub mod rows;
pub mod store;
pub mod timeline;
pub mod unread;

pub use rows::{CivilDate, Row};
pub use store::{Change, SidebarRow, Store};
pub use timeline::{Merge, PageAnchor, PostKey, Timeline};
pub use unread::{ChannelTotals, MarkUnread, MemberCounts, Threading, Unread, unread};
