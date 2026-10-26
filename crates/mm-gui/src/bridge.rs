//! The contract between the GUI and the engine.
//!
//! Exactly two channels cross the thread boundary, and the rule that makes the
//! whole design work is that **the engine holds no domain facts**. It owns
//! sockets, timers and in-flight bookkeeping; it never decides whether a
//! channel is unread. So every [`Event`] carries raw server data, and the GUI
//! thread feeds it to the one `mm_core::Store` that exists. "What does the app
//! think is true?" therefore has exactly one answer, on one thread, with no
//! lock around it.

use mm_api::ws::WsEvent;
use mm_core::Threading;
use mm_types::{Channel, ChannelId, ChannelMember, PostList, Team, User, UserId};

/// GUI to engine.
#[derive(Debug, Clone)]
pub enum Command {
    /// Load everything the sidebar needs.
    Sync,
    /// Fetch the newest page of a channel.
    OpenChannel(ChannelId),
    /// Post a message.
    Send { channel: ChannelId, text: String },
    /// Tell the server a channel has been read.
    MarkRead(ChannelId),
    /// Show a desktop notification.
    ///
    /// The *decision* is made on the GUI thread, which is the only place that
    /// has the store and therefore knows whether the channel is muted or
    /// already on screen. Only the D-Bus call crosses to the engine, so it
    /// never blocks a frame.
    Notify(mm_desktop::Notification),
    /// Stop.
    Quit,
}

/// Engine to GUI.
///
/// The size spread between a `Vec` of channels and a `Status(String)` is real,
/// but an event is produced, sent once through the channel and consumed by the
/// reducer. Boxing would add a pointer chase to every field access to save
/// nothing that is ever held in bulk.
///
/// Deliberately raw: these are server payloads, not conclusions. A variant
/// like `Event::ChannelBecameUnread` would mean the engine had started
/// deciding things, and then there would be two places that know what unread
/// means.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Event {
    /// Session established.
    Connected {
        server: String,
        me: UserId,
        username: String,
    },
    /// Whether collapsed reply threads apply, from the server config and the
    /// user preference. Never assumed.
    Threading(Threading),
    Teams(Vec<Team>),
    Channels(Vec<Channel>),
    Members(Vec<ChannelMember>),
    Users(Vec<User>),
    Posts {
        channel: ChannelId,
        page: PostList,
    },
    /// A live WebSocket event, already decoded.
    Ws(WsEvent),
    /// The WebSocket says a resync is needed: either it reconnected with a new
    /// connection id, or the event sequence had a hole.
    ResyncRequired(String),
    /// Something to show in the status line.
    Status(String),
    /// Something went wrong, in words meant for a person.
    Failed(String),
}

#[cfg(test)]
#[path = "tests/bridge.rs"]
mod tests;
