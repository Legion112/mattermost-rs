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
async fn handle(a: &Active, command: Command, events: &async_channel::Sender<Event>) {
    if refused_as_read_only(a, &command, events).await {
        return;
    }

    match command {
        Command::Sync => spawn_sync(a, events),
        Command::OpenChannel(channel) => spawn_open_channel(a, channel, events),
        Command::SetCategory(category) => set_category(a, &category, events).await,
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
        } => {
            let pending = format!("mmrs-{}", now_millis());
            let request =
                rest::create_post(&channel, &text, root.as_ref().map(PostId::as_str), &pending);
            // The `posted` event arrives over the WebSocket carrying the
            // same post, so nothing is applied here: letting one path own
            // it keeps the optimistic-post race out of this code entirely.
            if let Err(e) = fetch(&a.client, &request, rest::parse_post).await {
                let _ = events.send(Event::Failed(e)).await;
            }
        }
        Command::Notify(notification) => {
            // A missing notification daemon is a normal state on a minimal
            // desktop, and not a reason for a chat client to stop working.
            if let Err(e) = mm_desktop_xdg::show(&notification) {
                let _ = events.send(Event::Status(e)).await;
            }
        }
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
    let socket = tokio::spawn(socket_loop(
        client.clone(),
        session.token.clone(),
        events.clone(),
    ));

    Some(Active {
        client,
        session,
        socket,
        read_only: is_read_only(),
        collapsed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        downloads: Arc::new(tokio::sync::Semaphore::new(6)),
        warmed: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
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

    // DM labels come from the roster: a DM's own display_name is empty and its
    // name is the two user ids joined by `__`.
    if !dm_ids.is_empty() {
        dm_ids.sort();
        dm_ids.dedup();
        if let Ok(users) = fetch(client, &rest::users_by_ids(&dm_ids), rest::parse_users).await {
            snapshot.users.clone_from(&users);
            let _ = events.send(Event::Users(users)).await;
        }
    }
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
    let (stats, posts) = tokio::join!(
        fetch(client, &stats_request, rest::parse_channel_stats),
        fetch(client, &posts_request, rest::parse_post_list),
    );
    if let Ok(stats) = stats {
        let _ = events.send(Event::Stats(stats)).await;
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
}

/// Connect, read frames, and reconnect with backoff.
async fn socket_loop(client: Client, token: String, events: async_channel::Sender<Event>) {
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
                while let Some(frame) = socket.next_text().await {
                    let Ok(text) = frame else { break };
                    for outcome in protocol.on_frame(&text) {
                        match outcome {
                            WsOutcome::Event(event) => {
                                if events.send(Event::Ws(event)).await.is_err() {
                                    return;
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
                            WsOutcome::Hello { .. }
                            | WsOutcome::Reply { .. }
                            | WsOutcome::Undecodable { .. } => {}
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
