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
    collapsed: std::sync::atomic::AtomicBool,
    /// How many images may be downloading at once. Spawning one task per
    /// image keeps the command loop responsive; without a limit, opening a
    /// busy channel would open forty connections at once to somebody's
    /// work server.
    downloads: Arc<tokio::sync::Semaphore>,
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
        Command::Sync => sync(&a.client, &a.session, events, &a.collapsed).await,
        Command::OpenChannel(channel) => {
            open_channel(&a.client, &channel, events, a.collapsed()).await;
        }
        Command::Pin { post, pinned } => {
            best_effort(a, &rest::set_pinned(&post, pinned), events).await;
        }
        Command::Save { post, saved } => {
            let user = UserId::new(&a.session.user_id);
            best_effort(a, &rest::set_saved(&user, &post, saved), events).await;
        }
        Command::MarkUnread(post) => {
            let user = UserId::new(&a.session.user_id);
            let request = rest::set_unread(&user, &post, a.collapsed());
            best_effort(a, &request, events).await;
        }
        Command::Delete(post) => {
            // The removal reaches the store as a `post_deleted` event, so
            // nothing is applied here: one path owns it.
            best_effort(a, &rest::delete_post(&post), events).await;
        }
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
        sync(&a.client, &a.session, &events, &a.collapsed).await;
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
                    sync(&a.client, &a.session, &events, &a.collapsed).await;
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
        collapsed: std::sync::atomic::AtomicBool::new(false),
        downloads: Arc::new(tokio::sync::Semaphore::new(6)),
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
) {
    let config = match fetch(client, &rest::client_config(), rest::parse_client_config).await {
        Ok(config) => config,
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
            BTreeMap::new()
        }
    };
    let preferences = fetch(client, &rest::my_preferences(), rest::parse_preferences)
        .await
        .unwrap_or_default();
    let preference = |name: &str| {
        preferences
            .iter()
            .find(|p| p.category == "display" && p.name == name)
            .map(|p| p.value.as_str())
    };

    let _ = events.send(Event::Preferences(preferences.clone())).await;

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
) {
    let _ = events.send(Event::Status("loading…".into())).await;

    resolve_settings(client, events, collapsed).await;

    let teams = match fetch(client, &rest::my_teams(), rest::parse_teams).await {
        Ok(t) => t,
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
            return;
        }
    };

    let mut dm_ids: Vec<String> = Vec::new();
    for team in &teams {
        match fetch(client, &rest::my_channels(&team.id), rest::parse_channels).await {
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
                let _ = events.send(Event::Channels(channels)).await;
                // The grouping the user set up, so the sidebar matches the
                // one they see in every other Mattermost client. Addressed
                // by explicit user id: there is no `/users/me/` form.
                let user = UserId::new(&session.user_id);
                match fetch(
                    client,
                    &rest::my_categories(&user, &team.id),
                    rest::parse_categories,
                )
                .await
                {
                    Ok(list) => {
                        let _ = events.send(Event::Categories(list)).await;
                    }
                    // A sidebar grouped the server's way is better than one
                    // grouped ours, but one grouped ours is much better than
                    // none: the store falls back to a flat list.
                    Err(e) => {
                        let _ = events.send(Event::Status(format!("categories: {e}"))).await;
                    }
                }
            }
            Err(e) => {
                let _ = events.send(Event::Failed(e)).await;
            }
        }
        match fetch(
            client,
            &rest::my_channel_members(&team.id),
            rest::parse_channel_members,
        )
        .await
        {
            Ok(members) => {
                let _ = events.send(Event::Members(members)).await;
            }
            Err(e) => {
                let _ = events.send(Event::Failed(e)).await;
            }
        }
    }
    let _ = events.send(Event::Teams(teams)).await;

    // DM labels come from the roster: a DM's own display_name is empty and its
    // name is the two user ids joined by `__`.
    if !dm_ids.is_empty() {
        dm_ids.sort();
        dm_ids.dedup();
        if let Ok(users) = fetch(client, &rest::users_by_ids(&dm_ids), rest::parse_users).await {
            let _ = events.send(Event::Users(users)).await;
        }
    }
    let _ = events.send(Event::Status("ready".into())).await;
}

async fn open_channel(
    client: &Client,
    channel: &ChannelId,
    events: &async_channel::Sender<Event>,
    collapsed: bool,
) {
    // The member count for the header. Sent before the posts because it is a
    // small request and the header is the first thing that changes; a
    // failure is not worth reporting, since the header simply shows the
    // channel name without a count.
    if let Ok(stats) = fetch(
        client,
        &rest::channel_stats(channel),
        rest::parse_channel_stats,
    )
    .await
    {
        let _ = events.send(Event::Stats(stats)).await;
    }

    let request = rest::posts_for_channel(channel, &PostQuery::first(), collapsed);
    match fetch(client, &request, rest::parse_post_list).await {
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
