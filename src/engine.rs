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

use mm_api::rest::{self, PostQuery};
use mm_api::ws::{WsOutcome, WsProtocol};
use mm_api::{ApiError, HttpRequest, HttpResponse};
use mm_core::Threading;
use mm_gui::{Command, Event};
use mm_http::Client;
use mm_types::{ChannelId, UserId};

use crate::session::Session;

/// Start the engine on its own thread.
pub(crate) fn spawn(
    session: Session,
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

async fn run(
    session: Session,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: async_channel::Sender<Event>,
) {
    let client = match Client::new(&session.server) {
        Ok(c) => c.with_token(session.token.clone()),
        Err(e) => {
            let _ = events.send(Event::Failed(e.to_string())).await;
            return;
        }
    };

    let _ = events
        .send(Event::Connected {
            server: session.server.clone(),
            me: UserId::new(session.user_id.clone()),
            username: session.username.clone(),
        })
        .await;

    // The WebSocket runs as its own task so that a slow REST fetch cannot
    // stall event delivery -- and, more importantly, so the socket keeps being
    // polled. The server pings every 60s and closes after 100s without a pong,
    // so a connection nobody is reading dies while looking healthy.
    let socket_task = tokio::spawn(socket_loop(
        client.clone(),
        session.token.clone(),
        events.clone(),
    ));

    while let Some(command) = commands.recv().await {
        match command {
            Command::Quit => break,
            Command::Notify(notification) => {
                // A missing notification daemon is a normal state on a minimal
                // desktop, and not a reason for a chat client to stop working.
                if let Err(e) = mm_desktop_xdg::show(&notification) {
                    let _ = events.send(Event::Status(e)).await;
                }
            }
            Command::Sync => sync(&client, &session, &events).await,
            Command::OpenChannel(channel) => open_channel(&client, &channel, &events).await,
            Command::MarkRead(channel) => {
                // Best effort: the GUI has already updated optimistically, and
                // the store's monotonic joins mean a failure here cannot
                // corrupt anything -- the next fetch corrects it.
                let request = rest::view_channel(Some(&channel), None, true);
                let _ = client.send(&request).await;
            }
            Command::Send { channel, text } => {
                let pending = format!("mmrs-{}", now_millis());
                let request = rest::create_post(&channel, &text, None, &pending);
                match fetch(&client, &request, rest::parse_post).await {
                    // The `posted` event will arrive over the WebSocket and
                    // carry the same post, so nothing is applied here: letting
                    // one path own it keeps the optimistic-post race out of
                    // this code entirely.
                    Ok(_) => {}
                    Err(e) => {
                        let _ = events.send(Event::Failed(e)).await;
                    }
                }
            }
        }
    }

    socket_task.abort();
}

async fn sync(client: &Client, session: &Session, events: &async_channel::Sender<Event>) {
    let _ = events.send(Event::Status("loading…".into())).await;

    match fetch(client, &rest::client_config(), rest::parse_client_config).await {
        Ok(config) => {
            let threading = match config.get("CollapsedThreads").map(String::as_str) {
                Some("always_on" | "default_on") => Threading::Collapsed,
                _ => Threading::Flat,
            };
            let _ = events.send(Event::Threading(threading)).await;
        }
        Err(e) => {
            let _ = events.send(Event::Failed(e)).await;
        }
    }

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

async fn open_channel(client: &Client, channel: &ChannelId, events: &async_channel::Sender<Event>) {
    let request = rest::posts_for_channel(channel, &PostQuery::first());
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
