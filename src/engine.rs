//! The network thread.
//!
//! Owns the Tokio runtime, the HTTP client and the WebSocket. Holds **no
//! domain facts**: it fetches, decodes and forwards, and never decides whether
//! a channel is unread. Every conclusion is drawn on the GUI thread by the one
//! `mm_core::Store`.
//!
//! Channel choice is deliberate. GUI-to-engine is a `tokio` unbounded channel
//! because its `send` is not `async`, so a synchronous GTK callback can call
//! it. Engine-to-GUI is an `async_channel`, because its receiver is a future
//! that works under `glib::spawn_future_local`, and it is **bounded** so that
//! a large backfill applies backpressure instead of inflating the heap.
//!
//! The engine can start with no session at all. In that state it answers
//! nothing except [`Command::LogIn`], which is what lets the window show a
//! sign-in screen instead of demanding that a token already exist.

use mm_api::rest::{self, Login, PostQuery};
use mm_api::ws::{WsOutcome, WsProtocol};
use mm_api::{ApiError, HttpRequest, HttpResponse};
use mm_core::Threading;
use mm_gui::{Command, Event};
use mm_http::Client;
use mm_types::{ChannelId, PostId, UserId};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::session::Session;

/// Why the socket task stopped on its own, when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketEnd {
    /// The window has gone, or the socket URL could not be built: nothing
    /// to do about the session either way.
    Gone,
    /// The server no longer knows the token -- the session was revoked,
    /// expired, or the password was changed. Reconnecting cannot help.
    Unauthorized,
}

/// Where a request answered 401 says so, to wake the command loop.
///
/// A static rather than a field because `fetch` has only a client and is
/// called from everywhere, often on a task of its own -- so a flag read
/// after each command would miss a refusal that landed a moment later.
/// Each message carries the serial of the session the request was made
/// under; the loop ignores one for a session it has already put away,
/// so a slow download begun under a revoked token cannot sign out the
/// session that replaced it.
static REJECTED: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<u64>>> =
    std::sync::Mutex::new(None);

/// Counts sessions up, so a refusal can say which one it was for.
static SESSION_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn session_serial() -> u64 {
    SESSION_SERIAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// `serial` is the session the request was begun under, read before it
/// was sent.
fn note_status(status: u16, serial: u64) {
    if status != 401 {
        return;
    }
    let Ok(rejected) = REJECTED.lock() else {
        return;
    };
    if let Some(sender) = rejected.as_ref() {
        let _ = sender.send(serial);
    }
}

/// A signed-in connection.
struct Active {
    client: Client,
    session: Session,
    /// `None` once the task has ended and its reason has been read.
    socket: Option<tokio::task::JoinHandle<SocketEnd>>,
    /// Which session this is, for telling a late refusal of the last one
    /// from a refusal of this one.
    serial: u64,
    /// Refuse every command that would change something on the server.
    read_only: bool,
    /// Whether collapsed reply threads are in effect, which decides whether
    /// a page of posts is asked for as roots only. Learned at the first
    /// sync, so it is interior-mutable: every other field is fixed when the
    /// session opens and this one is not known until the config arrives.
    collapsed: Arc<std::sync::atomic::AtomicBool>,
    /// How many images may be downloading at once. Spawning one task per
    /// image keeps the command loop responsive; without a limit, opening a
    /// busy channel would open forty connections at once to somebody's
    /// work server.
    downloads: Arc<tokio::sync::Semaphore>,
    /// Channels whose first page has already been asked for, so a
    /// resync does not fetch the whole account again.
    warmed: Arc<tokio::sync::Mutex<std::collections::HashSet<ChannelId>>>,
    /// Whether the window has been handed what was remembered from last
    /// time. Once: a resync paints nothing from memory, since by then
    /// the window holds something newer than memory does.
    replayed: Arc<std::sync::atomic::AtomicBool>,
    /// The tray icon, where there is a tray to put one in. `None` on a
    /// desktop with no `StatusNotifierItem` host -- GNOME without an
    /// `AppIndicator` extension, most notably -- and the window is told,
    /// because closing to a tray that is not there would hide the
    /// application somewhere unreachable.
    tray: Option<mm_desktop_xdg::TrayHandle>,
    /// Where "somebody is typing" goes. The socket task owns the
    /// connection and is parked reading it, so telling the server
    /// means handing the frame to that task rather than writing it
    /// from here.
    typing: tokio::sync::mpsc::UnboundedSender<(ChannelId, String)>,
    /// What this client knows, written down between launches.
    ///
    /// Held here because the engine is what the server talks to: every
    /// page it fetches and every message the socket brings goes through
    /// it. The window's store is still the one that *decides* anything;
    /// this is a copy of what arrived, so the next launch can draw
    /// before it asks.
    held: Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    /// Whether `held` has anything in it that is not on disk yet.
    dirty: Arc<std::sync::atomic::AtomicBool>,
}

impl Active {
    fn collapsed(&self) -> bool {
        self.collapsed.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Start the engine on its own thread.
pub(crate) fn spawn(
    session: Option<Session>,
) -> (
    tokio::sync::mpsc::UnboundedSender<Command>,
    async_channel::Receiver<Event>,
) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let (evt_tx, evt_rx) = async_channel::bounded::<Event>(1_024);

    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = evt_tx.send_blocking(Event::Failed(format!(
                        "could not start the async runtime: {e}"
                    )));
                    return;
                }
            };
            runtime.block_on(run(session, cmd_rx, evt_tx));
        })
        .expect("the engine thread could not be started");

    (cmd_tx, evt_rx)
}

/// Whether this process may post anything to the server.
///
/// Set `MMRS_READ_ONLY=1` to read somebody's real server without putting
/// anything into it. Marking a channel read is deliberately **not** blocked:
/// it changes only the reader's own unread state, which is what reading a
/// channel is supposed to do. What is blocked is anything a colleague would
/// see -- a message, a reply, a reaction.
///
/// The guard lives in the engine because that is the one place every
/// command passes through.
fn is_read_only() -> bool {
    std::env::var_os(READ_ONLY).is_some_and(|v| v != "0" && !v.is_empty())
}

/// The environment variable that turns writing off.
pub(crate) const READ_ONLY: &str = "MMRS_READ_ONLY";

/// Refuse a command that would put something in front of another person,
/// when this process is observing rather than participating.
///
/// Returns whether the command was refused.
async fn refused_as_read_only(
    a: &Active,
    command: &Command,
    events: &async_channel::Sender<Event>,
) -> bool {
    if !a.read_only {
        return false;
    }
    let refusal = match command {
        Command::Send { .. } => "read-only: not sending",
        Command::React { .. } => "read-only: not reacting",
        Command::Pin { .. } => "read-only: not pinning",
        Command::Delete(_) => "read-only: not deleting",
        Command::Edit { .. } => "read-only: not editing",
        // Saving and marking unread change only the reader's own state,
        // like marking a channel read, so they are allowed.
        _ => return false,
    };
    let _ = events.send(Event::Status(refusal.into())).await;
    true
}

/// An image to fetch out of band.
enum Download {
    File {
        file: mm_types::FileId,
        preview: bool,
    },
    Avatar {
        user: UserId,
        version: mm_types::Timestamp,
    },
    /// A custom emoji's image. The name travels with it because that
    /// is what the window looks the picture up by; the id is only how
    /// it is fetched and cached.
    Emoji { name: String, id: mm_types::EmojiId },
    /// A link preview's picture, from somebody else's web server.
    Link { url: String },
}

/// Fetch an image without holding up the command loop.
///
/// Opening a channel with forty screenshots in it queued forty downloads
/// ahead of the next thing the reader clicked, so switching channels left
/// the previous channel's messages on screen for seconds. The semaphore
/// keeps that from becoming forty connections at once to somebody's work
/// server.
fn spawn_download(a: &Active, events: async_channel::Sender<Event>, what: Download) {
    let client = a.client.clone();
    let limit = Arc::clone(&a.downloads);
    tokio::spawn(async move {
        let Ok(_permit) = limit.acquire().await else {
            return;
        };
        // The cache first, so a channel full of screenshots -- or a
        // relaunch with a channel full of faces -- redraws without a
        // single request.
        match what {
            Download::File { file, preview } => {
                let cached = crate::avatars::load_file(&file);
                let image = if let Some(image) = cached {
                    image
                } else {
                    let Ok(image) = fetch(
                        &client,
                        &rest::file_contents(&file, preview),
                        rest::parse_image,
                    )
                    .await
                    else {
                        return;
                    };
                    crate::avatars::store_file(&file, &image);
                    image
                };
                let _ = events.send(Event::File { file, image }).await;
            }
            Download::Avatar { user, version } => {
                let cached = crate::avatars::load(&user, version);
                let image = if let Some(image) = cached {
                    image
                } else {
                    let Ok(image) =
                        fetch(&client, &rest::profile_image(&user), rest::parse_image).await
                    else {
                        return;
                    };
                    crate::avatars::store(&user, version, &image);
                    image
                };
                let _ = events.send(Event::Avatar { user, image }).await;
            }
            Download::Emoji { name, id } => {
                let cached = crate::avatars::load_emoji(&id);
                let image = if let Some(image) = cached {
                    image
                } else {
                    let Ok(image) =
                        fetch(&client, &rest::custom_emoji_image(&id), rest::parse_image).await
                    else {
                        return;
                    };
                    crate::avatars::store_emoji(&id, &image);
                    image
                };
                let _ = events.send(Event::EmojiImage { name, image }).await;
            }
            Download::Link { url } => {
                // No cache and no complaint: the card still reads
                // without its picture, and a dead image host must not
                // put an error in front of the reader.
                let Ok(image) = client.fetch_public(&url).await else {
                    return;
                };
                let _ = events.send(Event::LinkImage { url, image }).await;
            }
        }
    });
}

/// Load everything the sidebar needs, without holding up the commands the
/// reader is issuing meanwhile.
fn spawn_sync(a: &Active, events: &async_channel::Sender<Event>) {
    let client = a.client.clone();
    let session = a.session.clone();
    let events = events.clone();
    let collapsed = Arc::clone(&a.collapsed);
    let warmed = Arc::clone(&a.warmed);
    let first = !a.replayed.swap(true, std::sync::atomic::Ordering::Relaxed);
    let held = Arc::clone(&a.held);
    let dirty = Arc::clone(&a.dirty);
    tokio::spawn(async move {
        let channels = sync(&client, &session, &events, &collapsed, &held, &dirty, first).await;
        // Only once the reader has everything they asked for. Warming
        // first would put speculative requests in front of the sidebar.
        let collapsed = collapsed.load(std::sync::atomic::Ordering::Relaxed);
        warm_channels(
            &client, &warmed, collapsed, channels, &events, &held, &dirty,
        )
        .await;
    });
}

/// The commands that do one thing to one post.
///
/// None of them applies anything here: every one of them comes back
/// over the WebSocket -- a `post_edited`, a `post_deleted`, a changed
/// membership -- and that is the path that updates the window. Applying
/// the answer too would be two ways to learn the same fact, which is
/// how a client ends up disagreeing with itself.
async fn act_on_post(a: &Active, command: Command, events: &async_channel::Sender<Event>) {
    let user = UserId::new(&a.session.user_id);
    let request = match command {
        Command::Pin { post, pinned } => rest::set_pinned(&post, pinned),
        Command::Save { post, saved } => rest::set_saved(&user, &post, saved),
        Command::MarkUnread(post) => rest::set_unread(&user, &post, a.collapsed()),
        Command::Edit { post, message } => rest::edit_post(&post, &message),
        Command::Delete(post) => rest::delete_post(&post),
        // `handle` sends nothing else here.
        _ => return,
    };
    best_effort(a, &request, events).await;
}

/// Record a category as the window now draws it -- folded shut or open.
///
/// Split out only to keep `handle` short enough to read in one go.
async fn set_category(
    a: &Active,
    category: &mm_types::ChannelCategory,
    events: &async_channel::Sender<Event>,
) {
    let user = UserId::new(&a.session.user_id);
    let request = rest::update_category(&user, &category.team_id, category);
    best_effort(a, &request, events).await;
}

/// Upload whatever is attached, then post the message.
///
/// In that order because a post carries file *ids*, and the ids do not
/// exist until the bytes are on the server. An upload that fails stops
/// the send: posting the message without the file somebody attached
/// would be a worse answer than not posting it, because they would
/// have to notice and send it again.
async fn send_post(
    a: &Active,
    channel: &ChannelId,
    text: &str,
    root: Option<&PostId>,
    attach: &[mm_gui::bridge::Attachment],
    events: &async_channel::Sender<Event>,
) {
    let mut files = Vec::new();
    for attachment in attach {
        match upload_one(a, channel, attachment, events).await {
            Ok(id) => files.push(id),
            Err(e) => {
                let _ = events.send(Event::Failed(e)).await;
                return;
            }
        }
    }

    let pending = format!("mmrs-{}", now_millis());
    let request = rest::create_post(channel, text, root.map(PostId::as_str), &pending, &files);
    // The `posted` event arrives over the WebSocket carrying the same
    // post, so nothing is applied here: letting one path own it keeps
    // the optimistic-post race out of this code entirely.
    if let Err(e) = fetch(&a.client, &request, rest::parse_post).await {
        let _ = events.send(Event::Failed(e)).await;
    } else if !files.is_empty() {
        // The progress line, if any, has said its last word.
        let _ = events.send(Event::Status("ready".into())).await;
    }
}

/// Put one attachment on the server and give back its id.
///
/// A pasted image arrives as bytes and never touched the disk; a file
/// the reader picked is read now rather than when they picked it, so
/// what is sent is what the file says at the moment of sending.
async fn upload_one(
    a: &Active,
    channel: &ChannelId,
    attachment: &mm_gui::bridge::Attachment,
    events: &async_channel::Sender<Event>,
) -> Result<mm_types::FileId, String> {
    let name = attachment.name();
    let content = match attachment {
        mm_gui::bridge::Attachment::File(path) => {
            std::fs::read(path).map_err(|e| format!("cannot read {name}: {e}"))?
        }
        mm_gui::bridge::Attachment::Pasted { data, .. } => data.clone(),
        // Already there: nothing to send but the id.
        mm_gui::bridge::Attachment::Uploaded { id, .. } => return Ok(id.clone()),
    };
    if content.len() > UPLOAD_PIECE {
        return upload_in_pieces(a, channel, &name, content, events).await;
    }
    let request = rest::upload_file(channel, &name, content);
    let answer = fetch(&a.client, &request, rest::parse_upload).await?;
    answer
        .file_infos
        .into_iter()
        .next()
        .map(|info| info.id)
        .ok_or_else(|| format!("{name}: the server accepted it but named no file"))
}

/// A file bigger than one piece goes up as an upload session, a piece
/// at a time, with a word to the window after each: a screencast is
/// tens of megabytes and a bar that does not move reads as a hang.
/// The server names the file when the last byte is in and nothing
/// before; a session it never finishes naming is an error, not a
/// file with no id.
async fn upload_in_pieces(
    a: &Active,
    channel: &ChannelId,
    name: &str,
    content: Vec<u8>,
    events: &async_channel::Sender<Event>,
) -> Result<mm_types::FileId, String> {
    let total = u64::try_from(content.len()).unwrap_or(u64::MAX);
    let session = fetch(
        &a.client,
        &rest::create_upload(channel, name, total),
        rest::parse_upload_session,
    )
    .await?;
    let mut sent = 0_u64;
    for piece in content.chunks(UPLOAD_PIECE) {
        let done = fetch(
            &a.client,
            &rest::upload_chunk(&session.id, piece.to_vec()),
            rest::parse_chunk,
        )
        .await?;
        sent += u64::try_from(piece.len()).unwrap_or(0);
        let _ = events
            .send(Event::Uploading {
                name: name.to_owned(),
                sent,
                total,
            })
            .await;
        if let Some(file) = done {
            return Ok(file.id);
        }
    }
    Err(format!(
        "{name}: the server took every byte and named no file"
    ))
}

/// How much of a file goes up in one request. A megabyte is a few
/// seconds on a poor link, which is as long as a progress line should
/// stand still.
const UPLOAD_PIECE: usize = 1 << 20;

/// Put an attachment in the reader's download folder.
///
/// The cache first: a file already fetched to draw the row is the same
/// file, so keeping it costs no request. The name is the server's, with
/// anything that could climb out of the folder taken off -- a file name
/// is attacker-controlled, and `../../.bashrc` is a file name.
async fn save_file(
    a: &Active,
    file: &mm_types::FileId,
    name: &str,
    events: &async_channel::Sender<Event>,
) {
    let bytes = match crate::avatars::load_file(file) {
        Some(bytes) => bytes,
        None => match fetch(
            &a.client,
            &rest::file_contents(file, false),
            rest::parse_image,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = events.send(Event::Failed(format!("download: {e}"))).await;
                return;
            }
        },
    };
    match crate::downloads::save(name, &bytes) {
        Ok(path) => {
            let _ = events
                .send(Event::Status(format!("saved to {}", path.display())))
                .await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("download: {e}"))).await;
        }
    }
}

/// Fetch an attachment, if it is not already here, and tell the window
/// where a copy under its own name is, for the desktop to open.
async fn open_file(
    a: &Active,
    file: &mm_types::FileId,
    name: &str,
    events: &async_channel::Sender<Event>,
) {
    let bytes = match crate::avatars::load_file(file) {
        Some(bytes) => bytes,
        None => match fetch(
            &a.client,
            &rest::file_contents(file, false),
            rest::parse_image,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = events.send(Event::Failed(format!("open: {e}"))).await;
                return;
            }
        },
    };
    match crate::avatars::opened_copy(file, name, &bytes) {
        Ok(path) => {
            let _ = events.send(Event::FileReady(path)).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("open: {e}"))).await;
        }
    }
}

/// Put an icon in the tray, and turn its clicks into events.
///
/// `None` where no `StatusNotifierItem` host is listening, which the
/// window needs to know: without a tray, closing the window has to
/// quit rather than hide, or the application goes somewhere the reader
/// cannot get it back from.
///
/// The actions arrive on a plain channel from the tray's own thread,
/// so they are forwarded by a blocking task rather than awaited.
async fn spawn_tray(events: &async_channel::Sender<Event>) -> Option<mm_desktop_xdg::TrayHandle> {
    let (acts, from_tray) = std::sync::mpsc::channel();
    let handle = match mm_desktop_xdg::show_tray(acts).await {
        Ok(handle) => handle,
        Err(e) => {
            // Said once. A desktop with no tray host is a normal
            // state, but a tray that silently never appears looks
            // exactly like one that is broken.
            let _ = events.send(Event::Status(e)).await;
            return None;
        }
    };

    let out = events.clone();
    std::thread::spawn(move || {
        while let Ok(action) = from_tray.recv() {
            let event = match action {
                mm_desktop::TrayAction::Open => Event::Present,
                mm_desktop::TrayAction::Quit => Event::Quit,
            };
            if out.send_blocking(event).is_err() {
                return;
            }
        }
    });
    let _ = events.send(Event::TrayReady).await;
    Some(handle)
}

/// Show a notification, and go to the channel if it is clicked.
///
/// On a thread of its own, because waiting for somebody to click a
/// banner means waiting as long as the banner is up -- minutes, on a
/// shell that keeps them -- and the command loop has a client to run
/// meanwhile. One thread per notification, each ending when its banner
/// does.
///
/// A missing notification daemon is a normal state on a minimal
/// desktop and not a reason for a chat client to stop working, so the
/// failure goes to the status line and no further.
fn notify(notification: mm_desktop::Notification, events: &async_channel::Sender<Event>) {
    let channel = notification.channel.clone();
    let events = events.clone();
    std::thread::spawn(move || {
        match mm_desktop_xdg::show_and_wait(&notification) {
            Ok(true) => {
                // `Opened` is what the window already uses to go
                // somewhere and present itself, which is exactly what
                // clicking a notification should do.
                let _ = events.send_blocking(Event::Opened(channel));
            }
            Ok(false) => {}
            Err(e) => {
                let _ = events.send_blocking(Event::Status(e));
            }
        }
    });
}

/// Resolve a permalink and go there.
///
/// The link carries a post and nothing else, so the post is fetched
/// first: which channel it is in is a thing only the post knows. Then
/// the channel is opened the ordinary way, and the thread it belongs
/// to beside it, because a permalink usually points into a
/// conversation rather than at a lone message.
async fn follow_permalink(a: &Active, post: &PostId, events: &async_channel::Sender<Event>) {
    let found = match fetch(&a.client, &rest::one_post(post), rest::parse_post).await {
        Ok(found) => found,
        Err(e) => {
            let _ = events.send(Event::Failed(format!("link: {e}"))).await;
            return;
        }
    };
    let channel = found.channel_id.clone();
    let root = found.thread_root().clone();
    let collapsed = a.collapsed.load(std::sync::atomic::Ordering::Relaxed);
    open_channel(
        &a.client,
        &channel,
        events,
        collapsed,
        Some((&a.held, &a.dirty)),
    )
    .await;
    let _ = events.send(Event::Opened(channel)).await;
    if let Ok(page) = fetch(
        &a.client,
        &rest::post_thread(root.as_str()),
        rest::parse_post_list,
    )
    .await
    {
        let _ = events.send(Event::Thread { root, page }).await;
    }
}

/// Put the mention count on the dock icon.
///
/// A desktop with nothing listening for the signal simply ignores it,
/// so a failure here is not worth the reader's attention: it goes to
/// the status line and nowhere else.
async fn set_badge(a: &Active, mentions: i64, events: &async_channel::Sender<Event>) {
    if let Err(e) = mm_desktop_xdg::set_badge(mm_desktop::Badge::for_mentions(mentions)).await {
        let _ = events.send(Event::Status(e)).await;
    }
    // The same number, in the other place it belongs.
    if let Some(tray) = &a.tray {
        mm_desktop_xdg::update_tray(tray, mentions).await;
    }
}

/// Look for channels and people the client does not already know.
///
/// Both at once: they answer one question the reader asked, and two
/// round trips in series would make the quick switcher feel like it
/// was thinking. Failures are silent -- the local results are already
/// on screen, and an error line under them would say nothing useful
/// about a list that is merely shorter than it could be.
async fn find_elsewhere(
    a: &Active,
    team: &mm_types::TeamId,
    wanted: &str,
    events: &async_channel::Sender<Event>,
) {
    let channels = rest::search_channels(team, wanted);
    let archived = rest::search_archived_channels(team, wanted);
    let people = rest::search_users(team, wanted, ELSEWHERE_PEOPLE);
    let (channels, archived, people) = tokio::join!(
        fetch(&a.client, &channels, rest::parse_channel_search),
        fetch(&a.client, &archived, rest::parse_channel_search),
        fetch(&a.client, &people, rest::parse_user_search),
    );
    // The archived carry their delete_at, which is how the window tells
    // them apart; a server that refuses to show them answers an error,
    // which is simply none.
    let mut channels = channels.unwrap_or_default();
    channels.extend(archived.unwrap_or_default());
    let _ = events
        .send(Event::Elsewhere {
            term: wanted.to_owned(),
            channels,
            people: people.unwrap_or_default(),
        })
        .await;
}

/// Join a channel, and hand back enough for the sidebar to show it.
///
/// The membership comes from the join; the channel itself has to be
/// fetched, because a channel found by search was never in this
/// client's own list.
async fn join_channel(a: &Active, channel: &ChannelId, events: &async_channel::Sender<Event>) {
    let user = UserId::new(&a.session.user_id);
    let request = rest::join_channel(channel, &user);
    let member = match fetch(&a.client, &request, rest::parse_channel_member).await {
        Ok(member) => member,
        Err(e) => {
            let _ = events.send(Event::Failed(format!("join: {e}"))).await;
            return;
        }
    };
    match fetch(&a.client, &rest::one_channel(channel), rest::parse_channel).await {
        Ok(channel) => {
            let _ = events.send(Event::Joined { channel, member }).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("join: {e}"))).await;
        }
    }
}

/// Open the conversation with somebody, creating it if there is none.
///
/// The same call either way, so there is nothing to check first. The
/// membership is not returned, so the sidebar learns about it from the
/// sync that follows.
async fn open_direct(a: &Active, them: &UserId, events: &async_channel::Sender<Event>) {
    let me = UserId::new(&a.session.user_id);
    let request = rest::open_direct(&me, them);
    match fetch(&a.client, &request, rest::parse_channel).await {
        Ok(channel) => {
            let _ = events.send(Event::Channels(vec![channel.clone()])).await;
            open_channel(
                &a.client,
                &channel.id,
                events,
                a.collapsed.load(std::sync::atomic::Ordering::Relaxed),
                Some((&a.held, &a.dirty)),
            )
            .await;
            let _ = events.send(Event::Opened(channel.id)).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("direct: {e}"))).await;
        }
    }
}

/// Ask the server what matches.
///
/// Searching is per team in Mattermost -- there is no account-wide
/// endpoint -- and which team is a thing the window knows and the
/// engine deliberately does not.
async fn search(
    a: &Active,
    team: &mm_types::TeamId,
    terms: &str,
    page_no: u32,
    events: &async_channel::Sender<Event>,
) {
    let request = rest::search_posts(team, terms, page_no, SEARCH_PAGE);
    match fetch(&a.client, &request, rest::parse_search).await {
        Ok(page) => {
            // A full page is the only evidence that there is another
            // one: the server states no total.
            let full = page.order.len() >= SEARCH_PAGE as usize;
            let authors = authors_of(&page);
            let _ = events
                .send(Event::SearchResults {
                    terms: terms.to_owned(),
                    page_no,
                    full,
                    page,
                })
                .await;
            send_authors(&a.client, authors, events).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("search: {e}"))).await;
        }
    }
}

/// Everybody who wrote something in a page of posts.
fn authors_of(page: &mm_types::PostList) -> Vec<String> {
    let mut who: Vec<String> = page
        .posts
        .values()
        .map(|post| post.user_id.to_string())
        .filter(|id| !id.is_empty())
        .collect();
    who.sort();
    who.dedup();
    who
}

/// Fetch the people who wrote a page, so the window has names for
/// them.
///
/// A channel's page has always done this; search, pinned and saved
/// did not, and they are the three that reach across the whole team
/// -- so a hit from a channel this client has never opened drew the
/// author's raw id where a name should be.
async fn send_authors(client: &Client, who: Vec<String>, events: &async_channel::Sender<Event>) {
    if who.is_empty() {
        return;
    }
    if let Ok(users) = fetch(client, &rest::users_by_ids(&who), rest::parse_users).await {
        let _ = events.send(Event::Users(users)).await;
    }
}

/// A channel's pinned posts.
///
/// A failure is reported rather than swallowed. An empty pane and a
/// failed request look identical, and the drafts list already taught
/// this lesson once: it silently vanished for a week because the server
/// answered `null` and the parse error went nowhere.
async fn send_pinned(a: &Active, channel: ChannelId, events: &async_channel::Sender<Event>) {
    let request = rest::pinned_posts(&channel);
    match fetch(&a.client, &request, rest::parse_pinned).await {
        Ok(page) => {
            let authors = authors_of(&page);
            let _ = events.send(Event::PinnedPosts { channel, page }).await;
            send_authors(&a.client, authors, events).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("pinned: {e}"))).await;
        }
    }
}

/// Everything this reader has saved, across every channel.
async fn send_saved(a: &Active, events: &async_channel::Sender<Event>) {
    let user = UserId::new(&a.session.user_id);
    let request = rest::saved_posts(&user, SAVED_PAGE);
    match fetch(&a.client, &request, rest::parse_saved).await {
        Ok(page) => {
            let authors = authors_of(&page);
            let _ = events.send(Event::SavedPosts(page)).await;
            send_authors(&a.client, authors, events).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("saved: {e}"))).await;
        }
    }
}

/// Write several categories back together.
///
/// One request rather than two, because a channel moving between
/// categories leaves one and joins another: sent separately, there is a
/// window in which the server has it in both or in neither, and the
/// next sync would read that back as the truth.
async fn set_categories(
    a: &Active,
    categories: &[mm_types::ChannelCategory],
    events: &async_channel::Sender<Event>,
) {
    let Some(team) = categories.first().map(|c| c.team_id.clone()) else {
        return;
    };
    let user = UserId::new(&a.session.user_id);
    let request = rest::update_categories(&user, &team, categories);
    best_effort(a, &request, events).await;
}

/// Fetch the first page of the channels the reader is most likely to
/// open, while they are not asking for anything.
///
/// Ordered by last activity, which is the best guess available without
/// reading their mind: a channel that moved an hour ago is the one they
/// open next, and a channel silent since April is not. Unread ones come
/// first within that, for the same reason the sidebar sorts that way.
///
/// Nothing is applied differently from a click: the page arrives as the
/// same `Event::Posts`, the store joins it, and the window ignores it
/// because the channel is not the open one. The only difference is that
/// when the reader does click, the answer is already there.
async fn warm_channels(
    client: &Client,
    warmed: &Arc<tokio::sync::Mutex<std::collections::HashSet<ChannelId>>>,
    collapsed: bool,
    channels: Vec<mm_types::Channel>,
    events: &async_channel::Sender<Event>,
    held: &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
) {
    let mut wanted: Vec<mm_types::Channel> = channels
        .into_iter()
        // A channel nobody has ever posted in has nothing to warm.
        .filter(|c| !c.is_deleted() && c.last_post_at.millis() > 0)
        .collect();
    wanted.sort_by(|x, y| y.last_post_at.millis().cmp(&x.last_post_at.millis()));
    let total = wanted.len();
    if total > WARM_CEILING {
        let _ = events
            .send(Event::Status(format!(
                "warming the {WARM_CEILING} most recent of {total} channels"
            )))
            .await;
        wanted.truncate(WARM_CEILING);
    }

    let permits = Arc::new(tokio::sync::Semaphore::new(WARM_AT_ONCE));
    for (position, channel) in wanted.into_iter().enumerate() {
        {
            // Already asked for, this run or a previous sync.
            let mut warmed = warmed.lock().await;
            if !warmed.insert(channel.id.clone()) {
                continue;
            }
        }
        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
            return;
        };
        let client = client.clone();
        let events = events.clone();
        let held = Arc::clone(held);
        let dirty = Arc::clone(dirty);
        tokio::spawn(async move {
            let request = rest::posts_for_channel(&channel.id, &PostQuery::first(), collapsed);
            // Silent on failure, unlike a click: nobody asked for this,
            // so nobody should be told it did not work. The channel is
            // simply fetched the ordinary way when it is opened.
            if let Ok(page) = fetch(&client, &request, rest::parse_post_list).await {
                // Written down as well as drawn: this is the whole of
                // why a second launch has every channel in hand.
                keep(&held, &dirty, &channel.id, &page).await;
                let _ = events
                    .send(Event::Posts {
                        channel: channel.id.clone(),
                        page,
                    })
                    .await;
            }
            drop(permit);
        });
        // Brisk through the head, a trickle after it.
        tokio::time::sleep(if position < WARM_EAGER {
            WARM_GAP
        } else {
            WARM_GAP_TAIL
        })
        .await;
    }
}

/// Hand the window what the Threads and Drafts views are drawn from.
///
/// Neither is worth stopping the sync for -- the channel list beside
/// them is what the reader came for -- but a failure is said out loud.
/// Swallowing it meant a missing Drafts row was indistinguishable from
/// an account with no drafts, which is a bug that looks exactly like
/// working software, and is how a `null` body went unnoticed.
async fn send_views(
    client: &Client,
    threads: Result<mm_types::ThreadList, String>,
    drafts: Result<Vec<mm_types::Draft>, String>,
    events: &async_channel::Sender<Event>,
) {
    match threads {
        Ok(threads) => send_threads(client, threads, events).await,
        Err(e) => {
            let _ = events.send(Event::Status(format!("threads: {e}"))).await;
        }
    }
    match drafts {
        Ok(drafts) => {
            let _ = events.send(Event::Drafts(drafts)).await;
        }
        Err(e) => {
            let _ = events.send(Event::Status(format!("drafts: {e}"))).await;
        }
    }
}

/// Hand the window the followed threads, and the people in them.
///
/// A thread's root was written by somebody who need not be in any DM and
/// need not have posted in a channel this client has opened, so without
/// the second fetch the view lists raw user ids and the strip of faces
/// has nobody to draw.
async fn send_threads(
    client: &Client,
    threads: mm_types::ThreadList,
    events: &async_channel::Sender<Event>,
) {
    let mut who: Vec<String> = threads
        .threads
        .iter()
        .filter_map(|t| t.post.as_ref())
        .map(|p| p.user_id.to_string())
        .chain(
            threads
                .threads
                .iter()
                .flat_map(|t| &t.participants)
                .map(|u| u.id.to_string()),
        )
        .filter(|id| !id.is_empty())
        .collect();
    who.sort();
    who.dedup();
    let _ = events.send(Event::Threads(threads)).await;
    if who.is_empty() {
        return;
    }
    if let Ok(users) = fetch(client, &rest::users_by_ids(&who), rest::parse_users).await {
        let _ = events.send(Event::Users(users)).await;
    }
}

/// Fetch a channel's messages without holding up the next command.
///
/// Not awaited: a sync takes a couple of seconds, and a channel opened
/// while one is running -- which is every launch, since the sidebar paints
/// from a snapshot and picks a channel immediately -- would otherwise wait
/// for the whole of it.
fn spawn_open_channel(a: &Active, channel: ChannelId, events: &async_channel::Sender<Event>) {
    let client = a.client.clone();
    let events = events.clone();
    let collapsed = a.collapsed();
    let held = Arc::clone(&a.held);
    let dirty = Arc::clone(&a.dirty);
    tokio::spawn(async move {
        open_channel(&client, &channel, &events, collapsed, Some((&held, &dirty))).await;
    });
}

/// Send a request whose only interesting outcome is failure.
///
/// The server's answer is not applied: every one of these comes back over
/// the WebSocket, and letting one path own the change keeps the optimistic
/// update out of this code. A failure is worth saying out loud, though --
/// silently not pinning something is worse than saying so.
async fn best_effort(
    a: &Active,
    request: &mm_api::http::HttpRequest,
    events: &async_channel::Sender<Event>,
) {
    let serial = session_serial();
    match a.client.send(request).await {
        Ok(response) if response.status >= 400 => {
            note_status(response.status, serial);
            let _ = events
                .send(Event::Failed(format!(
                    "the server refused that ({})",
                    response.status
                )))
                .await;
        }
        Ok(_) => {}
        Err(e) => {
            let _ = events.send(Event::Failed(e.to_string())).await;
        }
    }
}

/// Carry out one command against an established session.
///
/// Split from `run` so the loop above stays about *when* a command may run —
/// before sign-in, after, at shutdown — and this stays about what each one
/// does.
// One match over every command there is, which is a dispatch table and
// reads as one. Splitting it to satisfy a line count would scatter the
// list of what this engine can be asked to do across several functions,
// and the exhaustiveness check -- the thing that makes adding a command
// safe -- would no longer be in one place.
#[allow(clippy::too_many_lines)]
async fn handle(a: &Active, command: Command, events: &async_channel::Sender<Event>) {
    if refused_as_read_only(a, &command, events).await {
        return;
    }

    match command {
        Command::Sync => spawn_sync(a, events),
        Command::OpenChannel(channel) => spawn_open_channel(a, channel, events),
        Command::Search { team, terms, page } => search(a, &team, &terms, page, events).await,
        Command::Pinned(channel) => send_pinned(a, channel, events).await,
        Command::Saved => send_saved(a, events).await,
        Command::Elsewhere { team, term } => find_elsewhere(a, &team, &term, events).await,
        Command::JoinChannel(channel) => join_channel(a, &channel, events).await,
        Command::OpenDirect(user) => open_direct(a, &user, events).await,
        Command::FollowPermalink(post) => follow_permalink(a, &post, events).await,
        Command::Badge(mentions) => set_badge(a, mentions, events).await,
        Command::FollowThread {
            team,
            root,
            following,
        } => {
            let me = UserId::new(&a.session.user_id);
            best_effort(
                a,
                &rest::follow_thread(&me, &team, &root, following),
                events,
            )
            .await;
        }
        Command::LeaveChannel(channel) => {
            let me = UserId::new(&a.session.user_id);
            // Said out loud rather than best-effort: the window has already
            // taken the channel off the sidebar, so a failure the reader is
            // not told about leaves them looking at a sidebar that disagrees
            // with the server until the next sync puts the row back.
            let request = rest::remove_from_channel(&channel, &me);
            if let Err(e) = fetch(&a.client, &request, |_| Ok(())).await {
                let _ = events.send(Event::Failed(format!("leave: {e}"))).await;
                let _ = events
                    .send(Event::ResyncRequired("leave failed".to_owned()))
                    .await;
            }
        }
        Command::AddToChannel { channel, user } => {
            let request = rest::join_channel(&channel, &user);
            if let Err(e) = fetch(&a.client, &request, rest::parse_channel_member).await {
                let _ = events.send(Event::Failed(format!("add: {e}"))).await;
            }
        }
        Command::RemoveFromChannel { channel, user } => {
            let request = rest::remove_from_channel(&channel, &user);
            if let Err(e) = fetch(&a.client, &request, |_| Ok(())).await {
                let _ = events.send(Event::Failed(format!("remove: {e}"))).await;
            }
        }
        Command::SetMuted { channel, muted } => {
            let me = UserId::new(&a.session.user_id);
            best_effort(a, &rest::set_channel_muted(&channel, &me, muted), events).await;
        }
        Command::SetCategory(category) => set_category(a, &category, events).await,
        Command::SetCategories(categories) => set_categories(a, &categories, events).await,
        Command::SetPreference {
            category,
            name,
            value,
        } => {
            let me = UserId::new(&a.session.user_id);
            best_effort(
                a,
                &rest::set_preference(&me, &category, &name, &value),
                events,
            )
            .await;
        }
        Command::Pin { .. }
        | Command::Save { .. }
        | Command::MarkUnread(_)
        | Command::Edit { .. }
        | Command::Delete(_) => act_on_post(a, command, events).await,
        Command::OpenThread(root) => {
            match fetch(
                &a.client,
                &rest::post_thread(root.as_str()),
                rest::parse_post_list,
            )
            .await
            {
                Ok(page) => {
                    let _ = events.send(Event::Thread { root, page }).await;
                }
                Err(e) => {
                    let _ = events.send(Event::Failed(e)).await;
                }
            }
        }
        Command::LoadOlder { channel, before } => {
            let query = PostQuery::Before {
                post: before.to_string(),
                per_page: BACKFILL_PAGE,
            };
            match fetch(
                &a.client,
                &rest::posts_for_channel(&channel, &query, a.collapsed()),
                rest::parse_post_list,
            )
            .await
            {
                Ok(page) => {
                    remember_page(a, &channel, &page).await;
                    let _ = events.send(Event::OlderPosts { channel, page }).await;
                }
                Err(e) => {
                    let _ = events.send(Event::Failed(e)).await;
                }
            }
        }
        Command::FetchFile { file, preview } => {
            spawn_download(a, events.clone(), Download::File { file, preview });
        }
        Command::FetchAvatar { user, version } => {
            spawn_download(a, events.clone(), Download::Avatar { user, version });
        }
        Command::SaveFile { file, name } => save_file(a, &file, &name, events).await,
        Command::OpenFile { file, name } => open_file(a, &file, &name, events).await,
        Command::Upload {
            channel,
            attachment,
            token,
        } => {
            let name = attachment.name();
            let answer = match upload_one(a, &channel, &attachment, events).await {
                Ok(id) => Event::Uploaded {
                    token,
                    file: mm_types::FileRef {
                        id,
                        name,
                        ..Default::default()
                    },
                },
                Err(why) => Event::UploadFailed { token, why },
            };
            let _ = events.send(answer).await;
        }
        Command::FileInfo(file) => {
            match fetch(&a.client, &rest::file_info(&file), rest::parse_file_info).await {
                Ok(info) => {
                    let _ = events.send(Event::FileInfo(info)).await;
                }
                Err(e) => {
                    let _ = events.send(Event::Failed(format!("file: {e}"))).await;
                }
            }
        }
        Command::SetChannelNotify { channel, desktop } => {
            let user = UserId::new(&a.session.user_id);
            let request = rest::set_channel_notify(&channel, &user, &desktop);
            best_effort(a, &request, events).await;
        }
        Command::PatchChannel {
            channel,
            display_name,
            header,
            purpose,
        } => {
            let request = rest::patch_channel(
                &channel,
                display_name.as_deref(),
                header.as_deref(),
                purpose.as_deref(),
            );
            best_effort(a, &request, events).await;
        }
        Command::ArchiveChannel(channel) => {
            best_effort(a, &rest::archive_channel(&channel), events).await;
        }
        Command::MakePrivate(channel) => {
            best_effort(a, &rest::set_channel_privacy(&channel, true), events).await;
        }
        Command::ReadAllThreads(team) => {
            let user = UserId::new(&a.session.user_id);
            best_effort(a, &rest::read_all_threads(&user, &team), events).await;
        }
        Command::ReadThread { team, root, upto } => {
            let user = UserId::new(&a.session.user_id);
            // One past the last reply: the endpoint marks read *up to*
            // the moment given, and the reply's own `create_at` leaves
            // that reply unread.
            let past = mm_types::Timestamp(upto.millis().saturating_add(1));
            best_effort(a, &rest::read_thread(&user, &team, &root, past), events).await;
        }
        Command::Statuses(users) => send_statuses(&a.client, &users, events).await,
        Command::MentionsLike {
            team,
            channel,
            prefix,
        } => {
            let request = rest::autocomplete_users(&team, &channel, &prefix, MENTIONS_ASKED);
            if let Ok(found) = fetch(&a.client, &request, rest::parse_autocomplete).await {
                // Not worth a complaint if it fails: the local names
                // are still offered, which is where this started.
                let _ = events
                    .send(Event::MentionsLike {
                        channel,
                        prefix,
                        in_channel: found.users,
                        elsewhere: found.out_of_channel,
                    })
                    .await;
            }
        }
        Command::Typing { channel, root } => {
            // Straight to the socket task, and nothing waits on it:
            // being a moment late with "is typing" costs nothing, and
            // a full queue means the socket is down, which the reader
            // already knows about.
            let parent = root.map(|root| root.to_string()).unwrap_or_default();
            a.typing.send((channel, parent)).ok();
        }
        Command::SaveDraft(draft) => {
            // An empty draft is a deletion. The server does the same
            // thing with one, but it answers 400 rather than quietly
            // dropping it, so the client has to choose the verb.
            let request = if draft.is_empty() {
                rest::delete_draft(
                    &UserId::new(&a.session.user_id),
                    &draft.channel_id,
                    (!draft.root_id.as_str().is_empty()).then_some(&draft.root_id),
                )
            } else {
                rest::upsert_draft(&draft)
            };
            best_effort(a, &request, events).await;
        }
        Command::SetPresence {
            presence,
            dnd_until,
        } => {
            match fetch(
                &a.client,
                &rest::set_status(&UserId::new(&a.session.user_id), presence, dnd_until),
                rest::parse_status,
            )
            .await
            {
                Ok(status) => {
                    let _ = events.send(Event::Statuses(vec![status])).await;
                }
                // Worth a complaint, unlike most of these: the reader
                // chose this and the dot not moving is the only sign
                // they would otherwise get.
                Err(e) => {
                    let _ = events
                        .send(Event::Failed(format!("could not set your status: {e}")))
                        .await;
                }
            }
        }
        Command::SetCustomStatus { status, duration } => {
            // Nothing to apply here on success: the server tells everybody,
            // this reader included, with a `user_updated` carrying the prop.
            if let Err(e) = fetch(
                &a.client,
                &rest::set_custom_status(&status, &duration),
                rest::parse_custom_status,
            )
            .await
            {
                let _ = events
                    .send(Event::Failed(format!(
                        "could not set your custom status: {e}"
                    )))
                    .await;
            }
        }
        Command::PatchProfile {
            first_name,
            last_name,
            nickname,
            position,
        } => {
            // Nothing applied here: the server tells everybody, this
            // reader included, with a user_updated.
            let request = rest::patch_user(
                &UserId::new(&a.session.user_id),
                first_name.as_deref(),
                last_name.as_deref(),
                nickname.as_deref(),
                position.as_deref(),
            );
            best_effort(a, &request, events).await;
        }
        Command::ClearCustomStatus => {
            best_effort(a, &rest::clear_custom_status(), events).await;
        }
        Command::FetchEmoji { name, id } => {
            spawn_download(a, events.clone(), Download::Emoji { name, id });
        }
        Command::FetchLinkImage(url) => {
            spawn_download(a, events.clone(), Download::Link { url });
        }
        Command::React { post, emoji, add } => {
            // Best effort, like MarkRead: the `reaction_added` event
            // comes back over the WebSocket and is what actually updates
            // the store, so a failure here simply leaves the chip as it
            // was rather than needing to be undone.
            let user = UserId::new(&a.session.user_id);
            let request = if add {
                rest::add_reaction(&user, &post, &emoji)
            } else {
                rest::remove_reaction(&user, &post, &emoji)
            };
            let _ = a.client.send(&request).await;
        }
        Command::MarkRead(channel) => {
            // Best effort: the GUI has already updated optimistically, and
            // the store's monotonic joins mean a failure here cannot
            // corrupt anything -- the next fetch corrects it.
            let request = rest::view_channel(Some(&channel), None, true);
            let _ = a.client.send(&request).await;
        }
        Command::Send {
            channel,
            text,
            root,
            attach,
        } => send_post(a, &channel, &text, root.as_ref(), &attach, events).await,
        Command::Notify(notification) => notify(notification, events),
        Command::LogIn { .. } | Command::Quit => unreachable!("handled above"),
    }
}

async fn run(
    session: Option<Session>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: async_channel::Sender<Event>,
) {
    let (rejected, mut rejections) = tokio::sync::mpsc::unbounded_channel();
    if let Ok(mut slot) = REJECTED.lock() {
        *slot = Some(rejected);
    }
    let mut active = match session {
        Some(saved) => activate(saved, &events).await,
        None => None,
    };
    if active.is_none() {
        let _ = events.send(Event::NeedLogin).await;
    } else if let Some(a) = &active {
        // Spawned, not awaited: the command loop below does not start
        // until this returns, so an awaited sync held every click the
        // reader made during it -- including the channel the window opens
        // for itself the moment the snapshot paints.
        spawn_sync(a, &events);
    }

    loop {
        // Either the next thing the window asks for, or the socket task
        // giving up: a revoked session shows up there first when the
        // window is idle, and in a refused request when it is not.
        let ended = async {
            match active.as_mut().and_then(|a| a.socket.as_mut()) {
                Some(socket) => socket.await,
                None => std::future::pending().await,
            }
        };
        let command = tokio::select! {
            command = commands.recv() => command,
            ended = ended => {
                if let Some(a) = active.as_mut() {
                    a.socket = None;
                }
                if matches!(ended, Ok(SocketEnd::Unauthorized)) {
                    sign_out(&mut active, &events).await;
                }
                continue;
            }
            Some(serial) = rejections.recv() => {
                if active.as_ref().is_some_and(|a| a.serial == serial) {
                    sign_out(&mut active, &events).await;
                }
                continue;
            }
        };
        let Some(command) = command else { break };
        // Signing in is the only thing that works without a session, and it is
        // ignored once there is one: a second sign-in while connected would
        // leave two sockets running.
        if let Command::LogIn {
            server,
            login_id,
            password,
            mfa,
        } = command
        {
            if active.is_some() {
                continue;
            }
            if let Some(saved) = authenticate(&server, &login_id, password, mfa, &events).await {
                active = activate(saved, &events).await;
                if let Some(a) = &active {
                    spawn_sync(a, &events);
                }
            }
            continue;
        }

        if matches!(command, Command::Quit) {
            break;
        }

        let Some(a) = &active else {
            // Anything else before sign-in is a stray click on a window that
            // has not connected yet.
            continue;
        };

        handle(a, command, &events).await;
    }
    if let Some(mut a) = active
        && let Some(socket) = a.socket.take()
    {
        socket.abort();
    }
}

/// How a sign-in attempt turned out.
pub(crate) enum LoginOutcome {
    Signed(Session),
    /// The credentials were accepted and the server wants a second factor.
    /// Not an error: the caller should ask for a code and try again.
    NeedsMfa,
    Failed(String),
}

/// Exchange a password for a session token.
///
/// **The one login implementation**, shared by the window and the `login`
/// subcommand. Two copies of this would drift, and the one nobody exercised
/// would be the one that broke.
///
/// The password arrives here, is used to build exactly one request, and is
/// dropped. It is never written to disk, never logged, and never placed in an
/// argument or the environment — only the token that comes back is persisted.
pub(crate) async fn attempt_login(
    server: &str,
    login_id: &str,
    password: String,
    mfa: Option<String>,
) -> LoginOutcome {
    let client = match Client::new(server) {
        Ok(c) => c,
        Err(e) => return LoginOutcome::Failed(e.to_string()),
    };

    let request = rest::login(&Login {
        login_id: login_id.to_owned(),
        password,
        mfa_token: mfa,
    });
    let response = match client.send(&request).await {
        Ok(r) => r,
        Err(e) => return LoginOutcome::Failed(format!("Could not reach {server}: {e}")),
    };

    match rest::parse_login(&response) {
        Ok(found) => LoginOutcome::Signed(Session {
            server: client.base().to_string(),
            token: found.token,
            user_id: found.user.id.to_string(),
            username: found.user.username.clone(),
        }),
        Err(ApiError::MfaRequired(_)) => LoginOutcome::NeedsMfa,
        Err(e) => LoginOutcome::Failed(explain(&e)),
    }
}

/// The window's wrapper: perform the attempt and report it as events.
async fn authenticate(
    server: &str,
    login_id: &str,
    password: String,
    mfa: Option<String>,
    events: &async_channel::Sender<Event>,
) -> Option<Session> {
    match attempt_login(server, login_id, password, mfa).await {
        LoginOutcome::Signed(saved) => {
            if let Err(e) = crate::session::save(&saved) {
                // The sign-in worked; only persisting it did not. Carrying on
                // means this session still works and only the next launch asks
                // again, which beats refusing a valid login.
                let _ = events.send(Event::Status(e)).await;
            }
            Some(saved)
        }
        LoginOutcome::NeedsMfa => {
            let _ = events
                .send(Event::LoginFailed {
                    message: String::new(),
                    needs_mfa: true,
                })
                .await;
            None
        }
        LoginOutcome::Failed(message) => {
            let _ = events
                .send(Event::LoginFailed {
                    message,
                    needs_mfa: false,
                })
                .await;
            None
        }
    }
}

/// Turn an API error into something worth showing a person.
pub(crate) fn explain(error: &ApiError) -> String {
    match error {
        ApiError::Unauthorized(_) => {
            "That email or password was not accepted. If your organisation uses single \
             sign-on, use a personal access token instead."
                .to_owned()
        }
        ApiError::RateLimited { retry_after_secs } => match retry_after_secs {
            Some(s) => format!("Too many attempts. Try again in {s} seconds."),
            None => "Too many attempts. Try again shortly.".to_owned(),
        },
        other => other.to_string(),
    }
}

/// Whether the server has stopped honouring the token.
///
/// Asked with the plainest request there is, so that a socket handshake
/// failing for network reasons is not mistaken for a revoked session:
/// only an answer of 401 counts, and no answer at all is the network.
async fn token_rejected(client: &Client) -> bool {
    match client.send(&rest::me()).await {
        Ok(response) => response.status == 401,
        Err(_) => false,
    }
}

/// Put the session away and tell the window to ask for a password again.
///
/// The stored token is removed as well: it is dead, and a client started
/// tomorrow with it would only find that out again. The server and the
/// name go to the window so the form has two of its three fields filled.
async fn sign_out(active: &mut Option<Active>, events: &async_channel::Sender<Event>) {
    let Some(mut a) = active.take() else { return };
    if let Some(socket) = a.socket.take() {
        socket.abort();
    }
    if let Err(e) = crate::session::forget() {
        let _ = events.send(Event::Status(e)).await;
    }
    let _ = events
        .send(Event::SessionEnded {
            server: a.session.server.clone(),
            login_id: a.session.username.clone(),
        })
        .await;
}

/// Build a client from a session and start its socket.
async fn activate(session: Session, events: &async_channel::Sender<Event>) -> Option<Active> {
    let client = match Client::new(&session.server) {
        Ok(c) => c.with_token(session.token.clone()),
        Err(e) => {
            let _ = events.send(Event::Failed(e.to_string())).await;
            return None;
        }
    };

    let _ = events
        .send(Event::Connected {
            server: session.server.clone(),
            me: UserId::new(session.user_id.clone()),
            username: session.username.clone(),
        })
        .await;

    // The WebSocket runs as its own task so a slow REST fetch cannot stall
    // event delivery -- and, more importantly, so the socket keeps being
    // polled. The server pings every 60s and closes after 100s without a pong,
    // so a connection nobody is reading dies while looking healthy.
    let (typing, typing_rx) = tokio::sync::mpsc::unbounded_channel();
    // Loaded before anything is fetched, so the window has the account
    // as this client last saw it rather than an empty one.
    let held = Arc::new(tokio::sync::Mutex::new(
        crate::snapshot::load(&session.server, &session.user_id).unwrap_or_default(),
    ));
    let dirty = Arc::new(std::sync::atomic::AtomicBool::new(false));
    keep_writing_down(&held, &dirty, &session);
    let warmed = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
    let socket = tokio::spawn(socket_loop(
        client.clone(),
        session.token.clone(),
        events.clone(),
        typing_rx,
        Some(Keeping {
            held: Arc::clone(&held),
            dirty: Arc::clone(&dirty),
            me: UserId::new(session.user_id.clone()),
            warmed: Arc::clone(&warmed),
        }),
    ));
    let socket = Some(socket);

    let tray = spawn_tray(events).await;
    Some(Active {
        client,
        session,
        socket,
        serial: SESSION_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
        read_only: is_read_only(),
        collapsed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        downloads: Arc::new(tokio::sync::Semaphore::new(6)),
        warmed,
        replayed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        tray,
        typing,
        held,
        dirty,
    })
}

/// How often the local state is written down.
///
/// Often enough that a client killed outright loses seconds of it, far
/// enough apart that a busy channel does not turn into a stream of
/// writes. Nothing is lost that the server does not still have.
const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Write the local state down whenever it has changed.
///
/// On its own task, and the write itself on a blocking one: this is a
/// few megabytes of JSON and the command loop should not stop for it.
fn keep_writing_down(
    held: &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
    session: &Session,
) {
    let held = Arc::clone(held);
    let dirty = Arc::clone(dirty);
    let (server, user) = (session.server.clone(), session.user_id.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SAVE_EVERY);
        loop {
            tick.tick().await;
            if !dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            let written = held.lock().await.clone();
            let (server, user) = (server.clone(), user.clone());
            let _ = tokio::task::spawn_blocking(move || {
                crate::snapshot::store(&server, &user, &written);
            })
            .await;
        }
    });
}

/// Keep the newest of a channel's messages in the local state.
///
/// Every page this client is given passes through here -- the one a
/// channel opens with, the ones warming fetches in the background, the
/// ones scrolling back asks for -- so what is written down is simply
/// what the server has said, newest first and bounded.
async fn remember_page(a: &Active, channel: &ChannelId, page: &mm_types::PostList) {
    keep(&a.held, &a.dirty, channel, page).await;
}

/// The same, for the background work that has no `Active` to hand.
async fn keep(
    store: &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
    channel: &ChannelId,
    page: &mm_types::PostList,
) {
    if page.posts.is_empty() {
        return;
    }
    let mut held = store.lock().await;
    let kept = held.conversations.entry(channel.clone()).or_default();
    for post in page.posts.values() {
        match kept.iter().position(|held| held.id == post.id) {
            Some(at) => kept[at] = post.clone(),
            None => kept.push(post.clone()),
        }
    }
    // Newest last, and only the tail: this is what a channel opens with,
    // not an archive of it.
    kept.sort_by(|a, b| a.create_at.cmp(&b.create_at).then_with(|| a.id.cmp(&b.id)));
    if kept.len() > crate::snapshot::KEPT_PER_CHANNEL {
        let drop = kept.len() - crate::snapshot::KEPT_PER_CHANNEL;
        kept.drain(..drop);
    }
    dirty.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Ask the server how it wants messages read, and tell the window.
///
/// Two settings decide that -- which name to show for a person, and
/// whether replies are collapsed -- and neither is a single value: the
/// server states a default and the user may hold a preference that
/// overrides it. The precedence lives in `mm_core::settings`, where it is
/// covered; this gathers the inputs.
async fn resolve_settings(
    client: &Client,
    events: &async_channel::Sender<Event>,
    collapsed: &std::sync::atomic::AtomicBool,
) -> (BTreeMap<String, String>, Vec<mm_types::Preference>) {
    // The config and the preferences are independent of each other, and
    // of everything before them.
    let config_request = rest::client_config();
    let preference_request = rest::my_preferences();
    let (config, preferences) = tokio::join!(
        fetch(client, &config_request, rest::parse_client_config),
        fetch(client, &preference_request, rest::parse_preferences),
    );
    let config = match config {
        Ok(config) => config,
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
            BTreeMap::new()
        }
    };
    let preferences = preferences.unwrap_or_default();
    // Only the three keys the two settings are resolved from. The whole
    // client config is a couple of hundred strings, and a snapshot that
    // carried all of it would be mostly things nothing reads.
    let config: BTreeMap<String, String> = SETTING_KEYS
        .iter()
        .filter_map(|k| config.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect();

    apply_settings(&config, &preferences, events, collapsed).await;
    (config, preferences)
}

/// How many of a channel's members to fetch when it opens, for `@`.
/// Mattermost's own autocomplete pages at a hundred; two hundred covers
/// every channel on this server with one request and is still one page.
const CHANNEL_ROSTER: u32 = 200;
/// How many hits one search asks for, and how many saved posts are
/// fetched. Both are a screenful and then some; neither pages yet.
/// How many search hits to ask for at a time.
///
/// A hundred, because that is the ceiling Mattermost's **database**
/// search has: measured against a live 12.0.1 server, `per_page` in the
/// body is ignored and every page comes back with at most a hundred
/// hits. Paging is honoured only where Elasticsearch or Bleve is
/// configured. Asking for sixty and being handed a hundred would make
/// the client's idea of a full page a lie in both directions.
const SEARCH_PAGE: u32 = 100;

/// How many names to ask the server for behind a half-typed `@`.
/// More than the popup shows, because the two lists it answers with
/// are merged with the local ones and then ranked again.
const MENTIONS_ASKED: u32 = 20;
const SAVED_PAGE: u32 = 60;
/// How many people one quick-switcher query asks about.
const ELSEWHERE_PEOPLE: u32 = 20;

/// How many messages to fetch when the reader scrolls off the top.
///
/// Smaller than the first page on purpose. Splicing a page into the list
/// model is the single most expensive thing this window does -- measured
/// at 67ms of CPU for sixty rows, before any of the layout and drawing
/// that follows -- and the cost is per row. Half the page is half the
/// stall, twice as often, and a hitch somebody does not notice twice is
/// better than one they notice once.
///
/// Not smaller still: each page is also a round trip, and below about
/// this many the window spends its time waiting on the server instead.
const BACKFILL_PAGE: u32 = 60;

/// How much of the account to warm in the background, and how fast.
///
/// A channel fetched only when it is clicked costs a round trip the
/// reader waits through, every first visit -- on a server a third of a
/// second away that is most of what opening a channel feels like. The
/// window does not freeze while it waits, which is almost worse: it
/// draws an empty pane, and an empty pane is indistinguishable from a
/// channel that failed to load.
///
/// So **every** channel is warmed, not a top slice of them. The slice
/// was the wrong shape of answer: it made the thirty channels somebody
/// uses most feel instant and left every other one feeling broken, and
/// which thirty those are is not something this client can know better
/// than the person clicking.
///
/// Pace rather than quantity is what keeps it polite, and the pace was
/// far too gentle. Measured against the official client on the same
/// account: its channel switches land in under a frame, ours in about
/// 1.9 seconds -- but a third of ours were *also* instant, and those
/// were the channels already warm. The slow ones were not GTK at all.
/// They were a round trip, because at four at a time with a quarter of
/// a second between starts, sixty channels take the better part of ten
/// seconds to ask for, and the reader is clicking during those ten
/// seconds.
///
/// Ten at a time and a sixtieth of a second between starts puts the
/// same sixty channels in hand in about two. The server multiplexes
/// them over one HTTP/2 connection and the official client prefetches
/// at least as hard; what this must not do is make somebody's work
/// server carry a burst without end, which is what `WARM_CEILING` is
/// for. A channel the reader *does* click is never queued behind the
/// guesses -- that request is issued on the click.
const WARM_EAGER: usize = 40;
const WARM_AT_ONCE: usize = 10;
const WARM_GAP: std::time::Duration = std::time::Duration::from_millis(15);
const WARM_GAP_TAIL: std::time::Duration = std::time::Duration::from_millis(60);

/// A ceiling, not a policy: an account in four figures of channels is
/// not a shape this has been measured against, and an unbounded loop
/// issuing requests on somebody's work server is the wrong thing to
/// find out with. Everything below it is warmed.
const WARM_CEILING: usize = 500;

/// How many followed threads to ask for. The view shows the ones that
/// have moved, and a reader with more than this many followed threads is
/// not going to read past it; the totals for the badge come back whole
/// however few are asked for.
const THREAD_PAGE: u32 = 50;

/// The client-config keys `apply_settings` reads.
const SETTING_KEYS: [&str; 4] = [
    "CollapsedThreads",
    "TeammateNameDisplay",
    "LockTeammateNameDisplay",
    "PostEditTimeLimit",
];

/// Work out how messages are to be read and tell the window, from config
/// and preferences that may have come either from the server or from the
/// snapshot. Shared so a snapshot-painted sidebar resolves them the same
/// way a synced one does -- in particular `collapsed`, which decides what
/// the first channel asks for and, got wrong, leaves most of a channel
/// out of the answer.
async fn apply_settings(
    config: &BTreeMap<String, String>,
    preferences: &[mm_types::Preference],
    events: &async_channel::Sender<Event>,
    collapsed: &std::sync::atomic::AtomicBool,
) {
    let preference = |name: &str| {
        preferences
            .iter()
            .find(|p| p.category == "display" && p.name == name)
            .map(|p| p.value.as_str())
    };

    let _ = events.send(Event::Preferences(preferences.to_vec())).await;

    let threading = mm_core::settings::threading(
        config.get("CollapsedThreads").map(String::as_str),
        preference("collapsed_reply_threads"),
    );
    collapsed.store(
        threading == Threading::Collapsed,
        std::sync::atomic::Ordering::Relaxed,
    );
    let _ = events.send(Event::Threading(threading)).await;

    let names = mm_core::settings::name_format(
        config.get("TeammateNameDisplay").map(String::as_str),
        config.get("LockTeammateNameDisplay").map(String::as_str) == Some("true"),
        preference("name_format"),
    );
    let _ = events.send(Event::NameFormat(names)).await;

    let limit = mm_core::settings::edit_limit(config.get("PostEditTimeLimit").map(String::as_str));
    let _ = events.send(Event::EditLimit(limit)).await;
}

async fn sync(
    client: &Client,
    session: &Session,
    events: &async_channel::Sender<Event>,
    collapsed: &std::sync::atomic::AtomicBool,
    held: &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
    first: bool,
) -> Vec<mm_types::Channel> {
    let _ = events.send(Event::Status("loading…".into())).await;

    // Last time's sidebar, painted now, so the window has something in it
    // while the requests below are in flight. It is a cache: everything
    // here is overwritten by what follows, and the prune at the end
    // removes whatever the server no longer has.
    // What is held in memory, not the file: the file is written on a
    // timer and the memory is current -- a channel this reader was just
    // taken out of is gone from memory and still in the file. And on
    // the first sync only. A resync replayed it too, and what it held
    // was a sync old: a category folded since came back unfolded in the
    // store for the moment between the replay and the fresh list, and a
    // click in that moment read the stale state and folded it again.
    // The lab caught it once in four sweeps.
    if first {
        let remembered = held.lock().await.clone();
        send_snapshot(events, remembered, collapsed).await;
    }

    let (config, preferences) = resolve_settings(client, events, collapsed).await;

    // Collected as it arrives and written down at the end, so the next
    // launch has a sidebar before it has a connection.
    let mut snapshot = crate::snapshot::Snapshot {
        preferences,
        config,
        ..crate::snapshot::Snapshot::default()
    };

    let teams = match fetch(client, &rest::my_teams(), rest::parse_teams).await {
        Ok(t) => t,
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
            return Vec::new();
        }
    };

    let mut dm_ids: Vec<String> = Vec::new();
    // Whether every team's channel list actually arrived. The prune below
    // deletes, so it may only run on a complete answer: one failed fetch
    // would otherwise empty the sidebar the snapshot had just filled.
    let mut complete = true;
    for team in &teams {
        // Together, not one after another. These three do not depend on
        // each other, and on a server a third of a second away the
        // difference between three round trips and one is most of what a
        // cold start felt like.
        // Bound to names first: a request built inside `join!` is a
        // temporary that does not live as long as the future holding it.
        let channel_request = rest::my_channels(&team.id);
        let member_request = rest::my_channel_members(&team.id);
        // Addressed by explicit user id: there is no `/users/me/` form
        // for categories.
        let me = UserId::new(&session.user_id);
        let category_request = rest::my_categories(&me, &team.id);
        // The followed threads and the unsent messages ride along: both
        // are per-team, both are one request, and both are wanted before
        // the reader can click the view that shows them.
        let thread_request = rest::my_threads(&me, &team.id, THREAD_PAGE);
        let draft_request = rest::my_drafts(&me, &team.id);
        let (channels, members, categories, threads, drafts) = tokio::join!(
            fetch(client, &channel_request, rest::parse_channels),
            fetch(client, &member_request, rest::parse_channel_members),
            fetch(client, &category_request, rest::parse_categories),
            fetch(client, &thread_request, rest::parse_threads),
            fetch(client, &draft_request, rest::parse_drafts),
        );
        send_views(client, threads, drafts, events).await;

        match channels {
            Ok(channels) => {
                for channel in &channels {
                    if channel.kind == mm_types::ChannelType::Direct {
                        dm_ids.extend(
                            channel
                                .name
                                .split("__")
                                .filter(|id| *id != session.user_id)
                                .map(str::to_owned),
                        );
                    }
                }
                snapshot.channels.extend(channels.iter().cloned());
                let _ = events.send(Event::Channels(channels)).await;
            }
            Err(e) => {
                complete = false;
                let _ = events.send(Event::Failed(e)).await;
            }
        }
        match members {
            Ok(members) => {
                snapshot.members.extend(members.iter().cloned());
                let _ = events.send(Event::Members(members)).await;
            }
            Err(e) => {
                let _ = events.send(Event::Failed(e)).await;
            }
        }
        match categories {
            Ok(list) => {
                snapshot.categories.push(list.clone());
                let _ = events.send(Event::Categories(list)).await;
            }
            // A sidebar grouped the server's way is better than one
            // grouped ours, but one grouped ours is much better than none:
            // the store falls back to a flat list.
            Err(e) => {
                let _ = events.send(Event::Status(format!("categories: {e}"))).await;
            }
        }
    }
    snapshot.teams.clone_from(&teams);
    let _ = events.send(Event::Teams(teams)).await;

    // The signed-in user's own record, which nothing else fetches:
    // `users_by_ids` is built from the DM partners and the reader is
    // on nobody's DM list but their own. It carries their
    // `notify_props`, which is how this client learns whether they
    // asked to be told about every message or only mentions -- and
    // whether to make a sound.
    if let Ok(me) = fetch(client, &rest::me(), rest::parse_user).await {
        snapshot.users.push(me.clone());
        let _ = events.send(Event::Users(vec![me])).await;
    }
    send_people(client, session, &mut dm_ids, &mut snapshot, events).await;
    // The server's own emoji. After the sidebar, because nothing on
    // screen waits for it: a message holding `:party_parrot:` reads as
    // `:party_parrot:` until the list lands and then becomes a picture.
    send_custom_emoji(client, events).await;

    // What the server actually has, now that it has all been asked for.
    // The only place allowed to remove a channel: every `apply_*` merges,
    // so a channel left since the snapshot was written would otherwise
    // stay in the sidebar for good. Only on a complete answer, and only
    // ever written down then -- a snapshot of a partial sync would make
    // the next launch wrong too.
    if complete {
        let _ = events
            .send(Event::ChannelsAre(
                snapshot.channels.iter().map(|c| c.id.clone()).collect(),
            ))
            .await;
        take_in_sidebar(held, dirty, snapshot.clone()).await;
    }

    let _ = events.send(Event::Status("ready".into())).await;
    snapshot.channels
}

/// Put what the sync learned into the local state, keeping what it did
/// not learn.
///
/// The conversations were filled in elsewhere -- by warming, by opening
/// a channel, by the socket -- and a sidebar refresh must not throw
/// them away. Writing is left to the task that writes it down.
async fn take_in_sidebar(
    held: &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
    fresh: crate::snapshot::Snapshot,
) {
    let mut held = held.lock().await;
    let conversations = std::mem::take(&mut held.conversations);
    *held = fresh;
    held.conversations = conversations;
    dirty.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Replay a snapshot as the events a sync would have sent, so the window
/// has one way to take this data in rather than two.
async fn send_snapshot(
    events: &async_channel::Sender<Event>,
    held: crate::snapshot::Snapshot,
    collapsed: &std::sync::atomic::AtomicBool,
) {
    apply_settings(&held.config, &held.preferences, events, collapsed).await;
    let _ = events.send(Event::Teams(held.teams)).await;
    let _ = events.send(Event::Channels(held.channels)).await;
    let _ = events.send(Event::Members(held.members)).await;
    for list in held.categories {
        let _ = events.send(Event::Categories(list)).await;
    }
    let _ = events.send(Event::Users(held.users)).await;

    // The conversations, before a single request has been made. This is
    // the difference between a client that can be used the moment it
    // opens and one that has to ask the server what it already knew.
    for (channel, posts) in held.conversations {
        if posts.is_empty() {
            continue;
        }
        let _ = events
            .send(Event::Posts {
                channel,
                page: as_page(posts),
            })
            .await;
    }
}

/// Messages written down earlier, shaped as the page the server would
/// have sent.
///
/// `prev_post_id` is the oldest message's own id rather than empty.
/// Empty is how the server says "there is nothing before this", and the
/// store believes it: a window that had been told a channel's history
/// was complete would never fetch the rest of it.
fn as_page(mut posts: Vec<mm_types::Post>) -> mm_types::PostList {
    posts.sort_by(|a, b| b.create_at.cmp(&a.create_at).then_with(|| b.id.cmp(&a.id)));
    let oldest = posts.last().map(|p| p.id.to_string()).unwrap_or_default();
    mm_types::PostList {
        order: posts.iter().map(|p| p.id.clone()).collect(),
        posts: posts.into_iter().map(|p| (p.id.clone(), p)).collect(),
        prev_post_id: oldest,
        ..mm_types::PostList::default()
    }
}

/// Ask for a batch of presences and pass on whatever comes back.
///
/// Silent on failure by design: a dot that says offline when somebody is
/// online is a smaller wrong than an error in front of the reader, and
/// the WebSocket corrects it the moment they do anything.
/// The people behind the DMs, and where everybody is.
///
/// DM labels come from the roster: a DM's own `display_name` is empty
/// and its `name` is the two user ids joined by `__`.
async fn send_people(
    client: &Client,
    session: &Session,
    dm_ids: &mut Vec<String>,
    snapshot: &mut crate::snapshot::Snapshot,
    events: &async_channel::Sender<Event>,
) {
    if !dm_ids.is_empty() {
        dm_ids.sort();
        dm_ids.dedup();
        if let Ok(users) = fetch(client, &rest::users_by_ids(dm_ids), rest::parse_users).await {
            snapshot.users.clone_from(&users);
            let _ = events.send(Event::Users(users)).await;
        }
    }
    // Who is at their desk, for the dots on the sidebar's faces --
    // **and this reader**, who is on no DM list of their own and whose
    // status the window draws on the control that sets it. Not worth a
    // complaint if it fails: every dot then reads offline, which is
    // what it read a moment ago anyway.
    let mut ids: Vec<UserId> = dm_ids.iter().map(UserId::new).collect();
    ids.push(UserId::new(&session.user_id));
    send_statuses(client, &ids, events).await;
}

async fn send_statuses(client: &Client, users: &[UserId], events: &async_channel::Sender<Event>) {
    if users.is_empty() {
        return;
    }
    if let Ok(statuses) = fetch(client, &rest::statuses_for(users), rest::parse_statuses).await {
        let _ = events.send(Event::Statuses(statuses)).await;
    }
}

/// Every custom emoji the server has, a page at a time.
///
/// Capped, deliberately. A server can have thousands, and the window
/// needs the list only to recognise a shortcode it is about to draw --
/// so the cost of the list is paid on every sync while the benefit is
/// paid on the handful that actually appear. Two hundred at a time up
/// to a thousand is every emoji on an ordinary server and a bounded
/// request on an extraordinary one; past that, the rest keep their
/// shortcodes, which is what they looked like before any of this.
async fn send_custom_emoji(client: &Client, events: &async_channel::Sender<Event>) {
    const PER_PAGE: u32 = 200;
    const PAGES: u32 = 5;

    let mut all = Vec::new();
    for page in 0..PAGES {
        let Ok(found) = fetch(
            client,
            &rest::custom_emoji(page, PER_PAGE),
            rest::parse_custom_emoji,
        )
        .await
        else {
            // Not worth a complaint: without the list every shortcode
            // stays a shortcode, which is where this started.
            break;
        };
        let short = found.len() < PER_PAGE as usize;
        all.extend(found);
        if short {
            break;
        }
    }
    if !all.is_empty() {
        let _ = events.send(Event::CustomEmoji(all)).await;
    }
}

async fn open_channel(
    client: &Client,
    channel: &ChannelId,
    events: &async_channel::Sender<Event>,
    collapsed: bool,
    keeping: Option<(
        &Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
        &Arc<std::sync::atomic::AtomicBool>,
    )>,
) {
    // Together: the member count and the messages do not depend on each
    // other, and a round trip saved here is one off every click on a
    // channel. A failed count is not worth reporting -- the header simply
    // shows the channel's name without one.
    let stats_request = rest::channel_stats(channel);
    let posts_request = rest::posts_for_channel(channel, &PostQuery::first(), collapsed);
    // Who is in it, which is who `@` may mean while reading it. One page
    // when the channel opens, rather than a request per keystroke the
    // way the web app does it: the answer is the same for every letter
    // typed, and it rides a round trip already being made.
    let members_request = rest::users_in_channel(channel, CHANNEL_ROSTER);
    let (stats, posts, members) = tokio::join!(
        fetch(client, &stats_request, rest::parse_channel_stats),
        fetch(client, &posts_request, rest::parse_post_list),
        fetch(client, &members_request, rest::parse_users_in_channel),
    );
    if let Ok(stats) = stats {
        let _ = events.send(Event::Stats(stats)).await;
    }
    // Not worth a complaint: without it `@` offers only the people who
    // have spoken, which is a smaller list rather than a broken one.
    let roster: Vec<UserId> = members
        .as_ref()
        .map(|members| members.iter().map(|u| u.id.clone()).collect())
        .unwrap_or_default();
    if let Ok(members) = members {
        let _ = events
            .send(Event::ChannelRoster {
                channel: channel.clone(),
                users: members,
            })
            .await;
    }

    match posts {
        Ok(page) => {
            // Post authors are often people not in any DM, so the roster has
            // to be filled from the page itself or every message reads as a
            // raw user id.
            let authors: Vec<String> = page
                .posts
                .values()
                .map(|p| p.user_id.to_string())
                .filter(|id| !id.is_empty())
                .collect();
            if let Some((held, dirty)) = keeping {
                keep(held, dirty, channel, &page).await;
            }
            let _ = events
                .send(Event::Posts {
                    channel: channel.clone(),
                    page,
                })
                .await;
            if !authors.is_empty() {
                let mut unique = authors;
                unique.sort();
                unique.dedup();
                if let Ok(users) =
                    fetch(client, &rest::users_by_ids(&unique), rest::parse_users).await
                {
                    let _ = events.send(Event::Users(users)).await;
                }
            }
        }
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
        }
    }

    // Last, once the messages are drawn: the dots are decoration, and a
    // round trip for them in front of the conversation would be felt.
    send_statuses(client, &roster, events).await;
}

/// Connect, read frames, and reconnect with backoff.
async fn socket_loop(
    client: Client,
    token: String,
    events: async_channel::Sender<Event>,
    mut typing: tokio::sync::mpsc::UnboundedReceiver<(ChannelId, String)>,
    keeping: Option<Keeping>,
) -> SocketEnd {
    let mut protocol = WsProtocol::new();
    let mut attempt: u32 = 0;

    loop {
        let url = match client.websocket_url(&protocol.reconnect_query(None)) {
            Ok(u) => u,
            Err(e) => {
                let _ = events.send(Event::Failed(e.to_string())).await;
                return SocketEnd::Gone;
            }
        };

        match mm_http::WsConn::connect(&url, &token).await {
            Ok(mut socket) => {
                attempt = 0;
                loop {
                    // One or the other, never both at once: the read
                    // borrows the socket for as long as it is waiting,
                    // so a frame to send is carried out of the select
                    // and written after it.
                    let outgoing = tokio::select! {
                        frame = socket.next_text() => {
                            let Some(Ok(text)) = frame else { break };
                            if !handle_frame(&mut protocol, &text, &events, keeping.as_ref())
                                .await
                            {
                                return SocketEnd::Gone;
                            }
                            None
                        }
                        said = typing.recv() => said,
                    };
                    if let Some((channel, parent)) = outgoing {
                        let frame = protocol.typing_frame(&channel, &parent);
                        // Nothing to report if it fails: the reader
                        // loses nothing, and the next frame reconnects.
                        if socket.send_text(frame).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = events.send(Event::Status(format!("websocket: {e}"))).await;
                // A handshake the server turns away looks the same as a
                // network that is down, from here. One plain request tells
                // them apart, and only the first is worth retrying.
                if token_rejected(&client).await {
                    return SocketEnd::Unauthorized;
                }
            }
        }
        // Full-jitter backoff, with the jitter supplied as a number so the
        // schedule stays a pure function in mm-api.
        //
        // Through u32 and `f64::from` rather than a cast: the value is already
        // bounded below 1000, so the conversion is lossless and the compiler
        // can see that it is.
        let jitter = f64::from(u32::try_from(now_millis() % 1_000).unwrap_or(0)) / 1_000.0;
        let wait = mm_api::ws::next_delay_secs(attempt, jitter);
        attempt = attempt.saturating_add(1);
        let _ = events
            .send(Event::Status(format!("reconnecting in {wait:.1}s")))
            .await;
        tokio::time::sleep(tokio::time::Duration::from_secs_f64(wait)).await;
    }
}

/// What the socket task writes down as frames arrive, and whose it is.
#[derive(Clone)]
struct Keeping {
    held: Arc<tokio::sync::Mutex<crate::snapshot::Snapshot>>,
    dirty: Arc<std::sync::atomic::AtomicBool>,
    me: UserId,
    /// Channels whose first page has been asked for: a channel left is
    /// taken out, so coming back warms it afresh.
    warmed: Arc<tokio::sync::Mutex<std::collections::HashSet<ChannelId>>>,
}

/// Decode one frame and pass on what it says. `false` means the window
/// has gone and the socket should stop.
async fn handle_frame(
    protocol: &mut WsProtocol,
    text: &str,
    events: &async_channel::Sender<Event>,
    keeping: Option<&Keeping>,
) -> bool {
    for outcome in protocol.on_frame(text) {
        match outcome {
            WsOutcome::Event(event) => {
                // A message that arrives while the window is open is as
                // much a part of what this client knows as a page it
                // fetched. Without this the local state would be
                // whatever was last fetched, and a restart would show a
                // channel missing everything said since.
                if let (Some(keeping), mm_api::ws::WsEvent::Posted { post, .. }) = (keeping, &event)
                {
                    let mut one = mm_types::PostList::default();
                    one.order.push(post.id.clone());
                    one.posts.insert(post.id.clone(), post.clone());
                    one.prev_post_id = post.id.to_string();
                    keep(&keeping.held, &keeping.dirty, &post.channel_id, &one).await;
                }
                // Taken out of a channel, this reader's copy of it goes too.
                // Kept, it came back on the next sync -- a sync replays what
                // is held -- and a rejoin then joined that stale run to the
                // fresh page through the one join notice both had, with
                // everything said while they were out missing between. The
                // lab found it, paging up through a channel it had been put
                // back into. Forgotten here, a rejoin fetches it afresh.
                if let (
                    Some(keeping),
                    mm_api::ws::WsEvent::UserRemovedFromChannel {
                        channel_id,
                        user_id,
                    },
                ) = (keeping, &event)
                    && *user_id == keeping.me
                {
                    keeping.held.lock().await.conversations.remove(channel_id);
                    keeping
                        .dirty
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    keeping.warmed.lock().await.remove(channel_id);
                }
                if events.send(Event::Ws(event)).await.is_err() {
                    return false;
                }
            }
            WsOutcome::Hello { lost_events, .. } if lost_events => {
                let _ = events
                    .send(Event::ResyncRequired(
                        "the server could not replay the gap".into(),
                    ))
                    .await;
            }
            WsOutcome::SequenceGap { expected, got } => {
                let _ = events
                    .send(Event::ResyncRequired(format!(
                        "event sequence jumped from {expected} to {got}"
                    )))
                    .await;
            }
            WsOutcome::Hello { .. } | WsOutcome::Reply { .. } | WsOutcome::Undecodable { .. } => {}
        }
    }
    true
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

async fn fetch<T>(
    client: &Client,
    request: &HttpRequest,
    parse: impl Fn(&HttpResponse) -> Result<T, ApiError>,
) -> Result<T, String> {
    let serial = session_serial();
    let response = client
        .send(request)
        .await
        .map_err(|e| format!("{}: {e}", request.route_key()))?;
    note_status(response.status, serial);
    parse(&response).map_err(|e| format!("{}: {e}", request.route_key()))
}
