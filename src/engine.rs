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

/// A signed-in connection.
struct Active {
    client: Client,
    session: Session,
    socket: tokio::task::JoinHandle<()>,
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
    tokio::spawn(async move {
        let channels = sync(&client, &session, &events, &collapsed).await;
        // Only once the reader has everything they asked for. Warming
        // first would put speculative requests in front of the sidebar.
        let collapsed = collapsed.load(std::sync::atomic::Ordering::Relaxed);
        warm_channels(&client, &warmed, collapsed, channels, &events).await;
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
        match upload_one(a, channel, attachment).await {
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
) -> Result<mm_types::FileId, String> {
    let name = attachment.name();
    let content = match attachment {
        mm_gui::bridge::Attachment::File(path) => {
            std::fs::read(path).map_err(|e| format!("cannot read {name}: {e}"))?
        }
        mm_gui::bridge::Attachment::Pasted { data, .. } => data.clone(),
    };
    let request = rest::upload_file(channel, &name, content);
    let answer = fetch(&a.client, &request, rest::parse_upload).await?;
    answer
        .file_infos
        .into_iter()
        .next()
        .map(|info| info.id)
        .ok_or_else(|| format!("{name}: the server accepted it but named no file"))
}

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
    open_channel(&a.client, &channel, events, collapsed).await;
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
    let people = rest::search_users(team, wanted, ELSEWHERE_PEOPLE);
    let (channels, people) = tokio::join!(
        fetch(&a.client, &channels, rest::parse_channel_search),
        fetch(&a.client, &people, rest::parse_user_search),
    );
    let _ = events
        .send(Event::Elsewhere {
            term: wanted.to_owned(),
            channels: channels.unwrap_or_default(),
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
            let _ = events
                .send(Event::SearchResults {
                    terms: terms.to_owned(),
                    page_no,
                    full,
                    page,
                })
                .await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(format!("search: {e}"))).await;
        }
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
            let _ = events.send(Event::PinnedPosts { channel, page }).await;
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
            let _ = events.send(Event::SavedPosts(page)).await;
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
) {
    let mut wanted: Vec<mm_types::Channel> = channels
        .into_iter()
        // A channel nobody has ever posted in has nothing to warm.
        .filter(|c| !c.is_deleted() && c.last_post_at.millis() > 0)
        .collect();
    wanted.sort_by(|x, y| y.last_post_at.millis().cmp(&x.last_post_at.millis()));
    wanted.truncate(WARM_CHANNELS);

    let permits = Arc::new(tokio::sync::Semaphore::new(WARM_AT_ONCE));
    for channel in wanted {
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
        tokio::spawn(async move {
            let request = rest::posts_for_channel(&channel.id, &PostQuery::first(), collapsed);
            // Silent on failure, unlike a click: nobody asked for this,
            // so nobody should be told it did not work. The channel is
            // simply fetched the ordinary way when it is opened.
            if let Ok(page) = fetch(&client, &request, rest::parse_post_list).await {
                let _ = events
                    .send(Event::Posts {
                        channel: channel.id.clone(),
                        page,
                    })
                    .await;
            }
            drop(permit);
        });
        tokio::time::sleep(WARM_GAP).await;
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
    tokio::spawn(async move {
        open_channel(&client, &channel, &events, collapsed).await;
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
    match a.client.send(request).await {
        Ok(response) if response.status >= 400 => {
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
        Command::SetMuted { channel, muted } => {
            let me = UserId::new(&a.session.user_id);
            best_effort(a, &rest::set_channel_muted(&channel, &me, muted), events).await;
        }
        Command::SetCategory(category) => set_category(a, &category, events).await,
        Command::SetCategories(categories) => set_categories(a, &categories, events).await,
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
                per_page: rest::DEFAULT_PER_PAGE,
            };
            match fetch(
                &a.client,
                &rest::posts_for_channel(&channel, &query, a.collapsed()),
                rest::parse_post_list,
            )
            .await
            {
                Ok(page) => {
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
        Command::SetPresence(presence) => {
            match fetch(
                &a.client,
                &rest::set_status(&UserId::new(&a.session.user_id), presence),
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

    while let Some(command) = commands.recv().await {
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
    if let Some(a) = active {
        a.socket.abort();
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
    let socket = tokio::spawn(socket_loop(
        client.clone(),
        session.token.clone(),
        events.clone(),
        typing_rx,
    ));

    let tray = spawn_tray(events).await;
    Some(Active {
        client,
        session,
        socket,
        read_only: is_read_only(),
        collapsed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        downloads: Arc::new(tokio::sync::Semaphore::new(6)),
        warmed: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        tray,
        typing,
    })
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

/// How much of the account to warm in the background, and how fast.
///
/// A channel fetched only when it is clicked costs a round trip the
/// reader waits through, every first visit -- on a server a third of a
/// second away that is most of what opening a channel feels like. So
/// the ones they are most likely to open are fetched while nothing else
/// is happening.
///
/// Bounded three ways, because speculative work on somebody's work
/// server should be polite: the most recently active channels only, two
/// requests at a time, and a gap between starts so a channel the reader
/// *does* click is never queued behind a burst of guesses.
const WARM_CHANNELS: usize = 30;
const WARM_AT_ONCE: usize = 2;
const WARM_GAP: std::time::Duration = std::time::Duration::from_millis(120);

/// How many followed threads to ask for. The view shows the ones that
/// have moved, and a reader with more than this many followed threads is
/// not going to read past it; the totals for the badge come back whole
/// however few are asked for.
const THREAD_PAGE: u32 = 50;

/// The client-config keys `apply_settings` reads.
const SETTING_KEYS: [&str; 3] = [
    "CollapsedThreads",
    "TeammateNameDisplay",
    "LockTeammateNameDisplay",
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
}

async fn sync(
    client: &Client,
    session: &Session,
    events: &async_channel::Sender<Event>,
    collapsed: &std::sync::atomic::AtomicBool,
) -> Vec<mm_types::Channel> {
    let _ = events.send(Event::Status("loading…".into())).await;

    // Last time's sidebar, painted now, so the window has something in it
    // while the requests below are in flight. It is a cache: everything
    // here is overwritten by what follows, and the prune at the end
    // removes whatever the server no longer has.
    if let Some(held) = crate::snapshot::load(&session.server, &session.user_id) {
        send_snapshot(events, held, collapsed).await;
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
        crate::snapshot::store(&session.server, &session.user_id, &snapshot);
    }

    let _ = events.send(Event::Status("ready".into())).await;
    snapshot.channels
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
) {
    let mut protocol = WsProtocol::new();
    let mut attempt: u32 = 0;

    loop {
        let url = match client.websocket_url(&protocol.reconnect_query(None)) {
            Ok(u) => u,
            Err(e) => {
                let _ = events.send(Event::Failed(e.to_string())).await;
                return;
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
                            if !handle_frame(&mut protocol, &text, &events).await {
                                return;
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

/// Decode one frame and pass on what it says. `false` means the window
/// has gone and the socket should stop.
async fn handle_frame(
    protocol: &mut WsProtocol,
    text: &str,
    events: &async_channel::Sender<Event>,
) -> bool {
    for outcome in protocol.on_frame(text) {
        match outcome {
            WsOutcome::Event(event) => {
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
    let response = client
        .send(request)
        .await
        .map_err(|e| format!("{}: {e}", request.route_key()))?;
    parse(&response).map_err(|e| format!("{}: {e}", request.route_key()))
}
