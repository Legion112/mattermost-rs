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
use mm_types::{
    CategoryList, Channel, ChannelId, ChannelMember, ChannelStats, PostId, PostList, Team,
    Timestamp, User, UserId,
};

/// GUI to engine.
#[derive(Debug, Clone)]
pub enum Command {
    /// Sign in with a password typed into the window.
    ///
    /// The password crosses this channel once and is used to build one
    /// request. It is never written to disk -- only the session token the
    /// server returns is -- and never logged, never an argument, never in the
    /// environment.
    LogIn {
        server: String,
        login_id: String,
        password: String,
        mfa: Option<String>,
    },
    /// Load everything the sidebar needs.
    Sync,
    /// Fetch the newest page of a channel.
    OpenChannel(ChannelId),
    /// Post a message, or a reply when `root` is set.
    Send {
        channel: ChannelId,
        text: String,
        root: Option<PostId>,
    },
    /// Load a thread so it can be read beside the channel.
    OpenThread(PostId),
    /// Tell the server a channel has been read.
    MarkRead(ChannelId),
    /// Fetch the page of history immediately before `before`, which is the
    /// oldest post currently held.
    LoadOlder { channel: ChannelId, before: PostId },
    /// Fetch one person's avatar. The window asks per author it has not
    /// drawn yet, and asks once: the engine answers with bytes either from
    /// its file cache or from the server.
    FetchAvatar {
        user: UserId,
        /// `last_picture_update`, which is the cache key.
        version: Timestamp,
    },
    /// Add or take back a reaction. `add` is decided by the window, which
    /// knows whether the signed-in user is already among them.
    React {
        post: PostId,
        emoji: String,
        add: bool,
    },
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
    /// There is no stored session, so the window should ask for one.
    NeedLogin,
    /// Signing in did not work.
    ///
    /// `needs_mfa` is separate from the message because it is not an error in
    /// the usual sense — the credentials were accepted and the server wants a
    /// second factor, so the window reveals the code field rather than
    /// painting the form red.
    LoginFailed {
        message: String,
        needs_mfa: bool,
    },
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
    /// The server's sidebar grouping for a team.
    Categories(CategoryList),
    /// A channel's member count, for its header.
    Stats(ChannelStats),
    /// One person's avatar, as encoded image bytes.
    Avatar {
        user: UserId,
        image: Vec<u8>,
    },
    Users(Vec<User>),
    Posts {
        channel: ChannelId,
        page: PostList,
    },
    /// A thread's root and replies.
    Thread {
        root: PostId,
        page: PostList,
    },
    /// A page of history older than what was held. Separate from `Posts`
    /// because it merges with a different anchor and must not move the view.
    OlderPosts {
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
