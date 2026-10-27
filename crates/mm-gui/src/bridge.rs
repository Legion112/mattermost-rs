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
    CategoryList, Channel, ChannelId, ChannelMember, ChannelStats, FileId, NameFormat, PostId,
    PostList, Team, Timestamp, User, UserId,
};

/// Something to attach to a message.
///
/// A pasted screenshot carries its **bytes** rather than a path to a
/// temporary file. Writing one would mean a file in the cache for
/// every paste, and something, somewhere, remembering to delete it --
/// which is a leak with extra steps. A file the reader picked stays a
/// path, because it is theirs and reading it twice is free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// A file on disk, which the reader chose.
    File(std::path::PathBuf),
    /// Bytes with a name, which never touched the disk.
    Pasted { name: String, data: Vec<u8> },
}

impl Attachment {
    /// What to call it on the server, and in the composer's tray.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("attachment")
                .to_owned(),
            Self::Pasted { name, .. } => name.clone(),
        }
    }
}

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
        /// What to upload and attach. Uploaded by the engine first,
        /// because a post carries file *ids* and the ids do not exist
        /// until the bytes are on the server.
        attach: Vec<Attachment>,
    },
    /// Load a thread so it can be read beside the channel.
    OpenThread(PostId),
    /// Pin a post to its channel, or take the pin off.
    Pin {
        post: PostId,
        pinned: bool,
    },
    /// Save a post, or take it out of the saved list.
    Save {
        post: PostId,
        saved: bool,
    },
    /// Mark a post, and everything after it, unread.
    MarkUnread(PostId),
    /// Delete a post. Only ever the reader's own.
    Delete(PostId),
    /// Change what a post says. Only ever the reader's own.
    Edit {
        post: PostId,
        message: String,
    },
    /// Tell the server a channel has been read.
    MarkRead(ChannelId),
    /// Fold a sidebar category shut, or open it again.
    ///
    /// Carries the whole category because that is what the server takes:
    /// it replaces a category rather than patching one.
    /// Search a team's messages for what was typed. The team comes from
    /// the window, because the engine holds no domain facts.
    Search {
        team: mm_types::TeamId,
        terms: String,
        /// Zero-based. Search has no cursor the way a channel's history
        /// does, so reaching the second page means asking for it by
        /// number.
        page: u32,
    },
    /// Fetch a channel's pinned posts.
    Pinned(ChannelId),
    /// Fetch this reader's saved posts.
    Saved,
    /// Ask the server for channels and people matching what is typed
    /// in the quick switcher -- including channels the reader has not
    /// joined, which are in no local list at all.
    Elsewhere {
        team: mm_types::TeamId,
        term: String,
    },
    /// Join a channel found that way.
    JoinChannel(ChannelId),
    /// Open the one-to-one conversation with somebody, creating it if
    /// there is not one yet.
    OpenDirect(UserId),
    /// Follow a permalink: find out which channel the post is in, open
    /// it, and open the post's thread beside it.
    FollowPermalink(PostId),
    /// Put the unread mention count on the application's dock icon.
    Badge(i64),
    /// Follow a thread, or stop following it. The team comes from the
    /// window, because the engine holds no domain facts.
    FollowThread {
        team: mm_types::TeamId,
        root: PostId,
        following: bool,
    },
    /// Write what is half-typed, so it survives a restart and follows
    /// the reader to their other clients. An empty one is a deletion.
    SaveDraft(mm_types::Draft),
    /// Ask the server who else matches a half-typed `@`.
    ///
    /// The store only knows the people this client has seen, which on
    /// a large server is a fraction of the team: without this,
    /// mentioning somebody you have not spoken to means typing their
    /// username in full and hoping.
    MentionsLike {
        team: mm_types::TeamId,
        channel: ChannelId,
        prefix: String,
    },
    /// Tell the channel somebody is writing. Over the WebSocket, not
    /// REST: it is an action on the socket, and the server forgets it
    /// after five seconds whether or not anything else is sent.
    Typing {
        channel: ChannelId,
        root: Option<PostId>,
    },
    /// Say where this reader is: online, away, do not disturb, offline.
    ///
    /// The server stops moving a status it was told, so choosing one is
    /// also choosing to keep it until it is changed again.
    SetPresence(mm_types::Presence),
    /// Mute a channel, or let it speak again.
    SetMuted {
        channel: ChannelId,
        muted: bool,
    },
    SetCategory(mm_types::ChannelCategory),
    /// Replace several categories at once, which is what moving a
    /// channel between two of them needs: the one it left and the one it
    /// joined have to change together.
    SetCategories(Vec<mm_types::ChannelCategory>),
    /// Fetch the page of history immediately before `before`, which is the
    /// oldest post currently held.
    LoadOlder {
        channel: ChannelId,
        before: PostId,
    },
    /// Fetch an image attached to a message. `preview` asks for the
    /// server's downscaled copy, which is what a message row wants.
    FetchFile {
        file: FileId,
        preview: bool,
    },
    /// Write an attachment into the reader's download folder, under
    /// the name the server gave it.
    SaveFile {
        file: FileId,
        name: String,
    },
    /// Fetch one custom emoji's image. Asked once per emoji the window
    /// has had to draw, by id, because a name can be freed and re-used
    /// while an id cannot.
    FetchEmoji {
        name: String,
        id: mm_types::EmojiId,
    },
    /// Fetch the picture a link preview offered, from whatever host the
    /// link points at. Asked once per URL, and never with a credential.
    FetchLinkImage(String),
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
    /// Which name to show for a person, resolved from the server's setting
    /// and the user's preference.
    NameFormat(NameFormat),
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
    /// A link preview's picture, keyed by the URL it was asked for.
    LinkImage {
        url: String,
        image: Vec<u8>,
    },
    /// An attached image, as encoded image bytes.
    File {
        file: FileId,
        image: Vec<u8>,
    },
    Users(Vec<User>),
    /// Where people are, for the dot on their face.
    Statuses(Vec<mm_types::Status>),
    /// The server's own custom emoji. Everything the window needs to
    /// turn `:party_parrot:` into a picture except the picture.
    CustomEmoji(Vec<mm_types::CustomEmoji>),
    /// One custom emoji's image, keyed by the name it is written as --
    /// which is what a reaction chip and a message hold, and what the
    /// window looks it up by.
    EmojiImage {
        name: String,
        image: Vec<u8>,
    },
    /// Who the server offered for a half-typed `@`, in the channel and
    /// outside it, with the prefix it answered so a reply to something
    /// already typed past can be ignored.
    MentionsLike {
        channel: ChannelId,
        prefix: String,
        in_channel: Vec<User>,
        elsewhere: Vec<User>,
    },
    /// Who is in one channel, from the roster fetched when it opened.
    ///
    /// Separate from `Users` because the channel is the point: `users`
    /// is everyone this client has ever seen, and `@` must offer the
    /// people in the conversation first.
    ChannelRoster {
        channel: ChannelId,
        users: Vec<User>,
    },
    /// The reader's preferences, which carry saved posts among much else.
    Preferences(Vec<mm_types::Preference>),
    /// The threads this reader follows, with the account-wide totals
    /// behind the sidebar badge.
    Threads(mm_types::ThreadList),
    /// This reader's unsent messages, from the server, so what was typed
    /// in another client is waiting here too.
    Drafts(Vec<mm_types::Draft>),
    /// Every channel the reader is in, as the server has just stated it.
    ///
    /// Sent at the end of a sync so the window can forget channels a
    /// snapshot had and the server does not: every other event merges,
    /// and a merge cannot remove.
    ChannelsAre(Vec<ChannelId>),
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
    /// What the server found for a quick-switcher query, with the term
    /// it answered so a reply to a query already moved on from can be
    /// ignored.
    Elsewhere {
        term: String,
        channels: Vec<Channel>,
        people: Vec<User>,
    },
    /// A channel the window should go to, because it was just created
    /// or joined on the reader's behalf.
    Opened(ChannelId),
    /// A channel the reader has just joined, with the membership that
    /// came back, so the sidebar has it before it is opened.
    Joined {
        channel: Channel,
        member: ChannelMember,
    },
    /// What a search answered, with the terms it answered, so a reply
    /// to a query the reader has already moved on from can be ignored.
    SearchResults {
        terms: String,
        /// Which page this is, so the window knows whether to replace
        /// what is on screen or add to it.
        page_no: u32,
        /// Whether the server filled the page, which is the only
        /// evidence there is that more exists.
        full: bool,
        page: PostList,
    },
    /// A channel's pinned posts.
    PinnedPosts {
        channel: ChannelId,
        page: PostList,
    },
    /// This reader's saved posts.
    SavedPosts(PostList),
    /// A live WebSocket event, already decoded.
    Ws(WsEvent),
    /// The WebSocket says a resync is needed: either it reconnected with a new
    /// connection id, or the event sequence had a hole.
    ResyncRequired(String),
    /// There is a tray, so closing the window may hide it rather than
    /// quit. Without this the window must quit on close, or the
    /// application goes somewhere the reader cannot get it back from.
    TrayReady,
    /// Bring the window to the front.
    Present,
    /// Leave, properly.
    Quit,
    /// Something to show in the status line.
    Status(String),
    /// Something went wrong, in words meant for a person.
    Failed(String),
}

#[cfg(test)]
#[path = "tests/bridge.rs"]
mod tests;
