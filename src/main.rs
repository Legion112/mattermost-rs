//! `mattermost-rs` — a native Linux Mattermost client.
//!
//! **Outside the coverage gate.** This crate is wiring: it constructs the real
//! transports and drives `mm-core`. Every decision it looks like it is making
//! is made in a gated crate.
//!
//! The GUI arrives in P1. Until then these subcommands exist so the stack can
//! be exercised end to end against a real server — which is a better test of
//! `mm-core` than any mock, because the server is the thing whose behaviour
//! the store is modelling.

mod avatars;
mod downloads;
mod engine;
mod geometry;
mod session;
mod snapshot;

use mm_api::ApiError;
use mm_api::rest::{self, PostQuery};
use mm_api::ws::{WsOutcome, WsProtocol};
use mm_core::{PageAnchor, Store, Threading};
use mm_http::Client;
use mm_types::{ChannelId, PostId, UserId};
use std::process::ExitCode;

const USAGE: &str = "\
mattermost-rs — a native Linux Mattermost client

usage:
  mattermost-rs login  --server <url> --login-id <id> --password-env <VAR> [--mfa <code>]
  mattermost-rs token  --server <url> --token <pat>
  mattermost-rs whoami
  mattermost-rs sync   [--channel <name>]
  mattermost-rs watch  [--seconds <n>]
  mattermost-rs send   --channel <name> --text <message>
  mattermost-rs react  --post <id> --emoji <name> [--remove]
  mattermost-rs history --channel <name> [--pages <n>]
  mattermost-rs doctor
  mattermost-rs gui
  mattermost-rs logout
  mattermost-rs --version | --help

Set MMRS_READ_ONLY=1 to read a server without putting anything into it:
sending and reacting are refused. Marking a channel read is still allowed,
since that changes only your own unread state.

A personal access token (`token`) is the supported path for SSO users: the
browser-based SSO flow needs a hand-off this client does not implement yet.
Passwords are read from an environment variable, never from an argument, so
they do not land in your shell history or in `ps` output.
";

/// Split argv into a command and its arguments.
///
/// No arguments means "open the app", which is what launching from the desktop
/// entry does.
///
/// Separated out and tested because the obvious spelling of the tail is
/// `&args[1..]`, which panics on an empty slice — so running the binary with
/// no arguments at all, the single most common way anybody starts it, crashed
/// before reaching any of the code that had been tested.
fn split_args(args: &[String]) -> (&str, &[String]) {
    let first = args.first().map_or("gui", String::as_str);
    // A `mattermost://` URL is not a subcommand: it is what the desktop
    // hands this binary when somebody clicks a link, and it has to
    // reach the window. Without this the `.desktop` entry's handler
    // registration is accepted and every link prints "unknown
    // command", which is worse than not registering at all.
    let command = if first.starts_with(DEEP_LINK_SCHEME) || first == HIDDEN_FLAG {
        "gui"
    } else {
        first
    };
    (command, args.get(1..).unwrap_or(&[]))
}

/// The scheme the `.desktop` entry registers for.
const DEEP_LINK_SCHEME: &str = "mattermost://";

/// Start in the tray rather than with a window. What the autostart
/// entry passes, so logging in does not throw a window at somebody.
const HIDDEN_FLAG: &str = "--hidden";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest_args) = split_args(&args);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = match command {
        "--version" | "-V" => {
            println!("mattermost-rs {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "login" => runtime.block_on(login(rest_args)),
        "token" => runtime.block_on(store_token(rest_args)),
        "whoami" => runtime.block_on(whoami()),
        "sync" => runtime.block_on(sync(rest_args)),
        "watch" => runtime.block_on(watch(rest_args)),
        "send" => runtime.block_on(send(rest_args)),
        "react" => runtime.block_on(react(rest_args)),
        "history" => runtime.block_on(history(rest_args)),
        "doctor" => doctor(),
        "gui" => gui(args.iter().any(|a| a == HIDDEN_FLAG)),
        "logout" => session::clear(),
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

/// Find a channel by its URL name, in whichever team has it.
async fn resolve_channel(client: &Client, name: &str) -> Result<ChannelId, String> {
    let teams = fetch(client, &rest::my_teams(), rest::parse_teams).await?;
    for team in &teams {
        let channels = fetch(client, &rest::my_channels(&team.id), rest::parse_channels).await?;
        if let Some(c) = channels.into_iter().find(|c| c.name == name) {
            return Ok(c.id);
        }
    }
    Err(format!("no channel called {name:?}"))
}

/// Walk a channel's scrollback backwards, the way the window does when the
/// reader reaches the top.
///
/// This exists to exercise the part of backfill that no unit test reaches:
/// that `before=` returns what the server says it does, and that merging
/// those pages with `PageAnchor::OlderThanHeld` produces one timeline with
/// no gap and no duplicates. The window's half of it -- restoring the
/// reading position after a prepend -- needs a human at the scroll wheel.
async fn history(args: &[String]) -> Result<(), String> {
    let name = require(args, "--channel")?;
    let pages: usize = match flag(args, "--pages") {
        Some(n) => n
            .parse()
            .map_err(|_| format!("--pages {n:?} is not a number"))?,
        None => 3,
    };
    let (client, saved) = connect()?;
    let channel = resolve_channel(&client, &name).await?;
    let mut store = Store::new(UserId::new(saved.user_id.clone()), Threading::Flat);

    let first = fetch(
        &client,
        // Flat on purpose: this subcommand checks that pages merge into
        // one ordered timeline, and the flat view is the one with the most
        // posts in it to merge.
        &rest::posts_for_channel(&channel, &PostQuery::first(), false),
        rest::parse_post_list,
    )
    .await?;
    store.apply_post_list(&channel, &first, PageAnchor::Newest);
    println!(
        "page 1: {} post(s) held",
        store.channel_posts(&channel).len()
    );

    for page in 2..=pages {
        if store.older_complete(&channel) {
            println!("reached the beginning of the channel");
            break;
        }
        let Some(oldest) = store.channel_posts(&channel).first().map(|p| p.id.clone()) else {
            break;
        };
        let query = PostQuery::Before {
            post: oldest.to_string(),
            per_page: rest::DEFAULT_PER_PAGE,
        };
        let older = fetch(
            &client,
            &rest::posts_for_channel(&channel, &query, false),
            rest::parse_post_list,
        )
        .await?;
        store.apply_post_list(&channel, &older, PageAnchor::OlderThanHeld);
        println!(
            "page {page}: {} post(s) held, oldest is {:?}",
            store.channel_posts(&channel).len(),
            store
                .channel_posts(&channel)
                .first()
                .map(|p| p.message.clone())
                .unwrap_or_default()
        );
    }

    // The whole point: one run of posts, strictly ordered, no repeats.
    let held = store.channel_posts(&channel);
    let ordered = held.windows(2).all(|w| w[0].create_at <= w[1].create_at);
    let mut ids: Vec<&str> = held.iter().map(|p| p.id.as_str()).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    println!(
        "
{total} post(s), ordered={ordered}, distinct={}",
        ids.len()
    );
    if !ordered || ids.len() != total {
        return Err("the merged timeline is out of order or has duplicates".into());
    }
    Ok(())
}

/// Add or take back a reaction.
///
/// The window does this from a chip under the message; this is the same two
/// requests from a terminal, and it is how their shapes were checked against
/// a real server -- the remove path puts the emoji in the URL, which no unit
/// test can prove the server accepts.
async fn react(args: &[String]) -> Result<(), String> {
    refuse_if_read_only("react")?;
    let post = PostId::new(require(args, "--post")?);
    let emoji = require(args, "--emoji")?;
    let remove = args.iter().any(|a| a == "--remove");
    let (client, saved) = connect()?;
    let me = UserId::new(&saved.user_id);

    if remove {
        let response = client
            .send(&rest::remove_reaction(&me, &post, &emoji))
            .await
            .map_err(|e| e.to_string())?;
        if response.status >= 400 {
            return Err(format!("could not remove :{emoji}: ({})", response.status));
        }
        println!("took back :{emoji}: on {post}");
    } else {
        let added = fetch(
            &client,
            &rest::add_reaction(&me, &post, &emoji),
            rest::parse_reaction,
        )
        .await?;
        println!("reacted :{}: to {}", added.emoji_name, added.post_id);
    }
    Ok(())
}

/// Report what the desktop can actually do.
///
/// Notifications are the part most likely to be silently missing: a desktop
/// with no notification daemon looks exactly like a bug in the notification
/// rules. This is the manual check the plan calls for, since CI has no session
/// bus and never will.
fn doctor() -> Result<(), String> {
    println!("mattermost-rs {}", env!("CARGO_PKG_VERSION"));
    let mut problems: Vec<String> = Vec::new();

    if let Ok(saved) = session::load() {
        println!("session:       {} on {}", saved.username, saved.server);
    } else {
        println!("session:       none");
        problems.push("no session — run `token` or `login`".into());
    }

    if mm_desktop_xdg::is_available() {
        println!("notifications: a daemon is listening");
        let probe = mm_desktop::Notification {
            title: "mattermost-rs".into(),
            body: "Notifications are working.".into(),
            channel: mm_types::ChannelId::new("doctor"),
            is_mention: false,
            // On purpose: `doctor` is run to find out whether a
            // banner appears, and a sound is the other half of the
            // answer.
            sound: true,
        };
        if let Err(e) = mm_desktop_xdg::show(&probe) {
            println!("               the daemon refused it: {e}");
            problems.push(e);
        } else {
            println!("               sent a test notification — did one appear?");
        }
    } else {
        println!("notifications: no daemon is listening");
        problems.push("no notification daemon; this client will never notify".into());
    }

    if problems.is_empty() {
        println!("\neverything checks out");
        return Ok(());
    }
    Err(format!(
        "\n{} problem(s): {}",
        problems.len(),
        problems.join("; ")
    ))
}

/// Launch the window.
///
/// The GTK main loop owns this thread from here on; the engine runs on its
/// own. Note what is NOT done: no `block_on` around the GUI, and the async
/// runtime built above is dropped first, because two runtimes competing for
/// the same thread is a deadlock waiting to happen.
fn gui(hidden: bool) -> Result<(), String> {
    // A missing session is not an error any more: the window shows a sign-in
    // screen. Demanding a token before the GUI will even open was a developer's
    // idea of a login flow.
    let saved = session::load().ok();
    let (commands, events) = engine::spawn(saved);
    let code = mm_gui::run(mm_gui::Wiring {
        commands,
        events,
        restored: geometry::load(),
        hidden,
        remember: std::rc::Rc::new(|state| geometry::store(&state)),
    });
    if code == mm_gui::glib_exit_success() {
        Ok(())
    } else {
        Err("the window exited with an error".to_owned())
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).cloned()
}

/// Stop a subcommand that would put something in front of another person.
///
/// The window's guard lives in the engine, which the command line does not
/// go through, so it needs its own. `MMRS_READ_ONLY` has to mean the same
/// thing for the whole process or it means nothing.
fn refuse_if_read_only(what: &str) -> Result<(), String> {
    if std::env::var_os(engine::READ_ONLY).is_some_and(|v| v != "0" && !v.is_empty()) {
        return Err(format!(
            "{what} would put something in front of another person, and {} is set",
            engine::READ_ONLY
        ));
    }
    Ok(())
}

fn require(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("{name} is required\n\n{USAGE}"))
}

// ---------------------------------------------------------------- auth

async fn login(args: &[String]) -> Result<(), String> {
    let server = require(args, "--server")?;
    let login_id = require(args, "--login-id")?;
    let var = require(args, "--password-env")?;
    let password =
        std::env::var(&var).map_err(|_| format!("the environment variable {var} is not set"))?;

    // The same attempt the window makes. One login implementation, so testing
    // either exercises both.
    match engine::attempt_login(&server, &login_id, password, flag(args, "--mfa")).await {
        engine::LoginOutcome::Signed(saved) => {
            session::save(&saved)?;
            println!("logged in as {} on {}", saved.username, saved.server);
            Ok(())
        }
        engine::LoginOutcome::NeedsMfa => {
            Err("this account needs a multi-factor code: re-run with --mfa <code>".to_owned())
        }
        engine::LoginOutcome::Failed(message) => Err(format!("login failed: {message}")),
    }
}

async fn store_token(args: &[String]) -> Result<(), String> {
    let server = require(args, "--server")?;
    let token = require(args, "--token")?;
    let client = Client::new(&server)
        .map_err(|e| e.to_string())?
        .with_token(token.clone());

    // Prove the token works before saving it, so a typo fails here rather
    // than at every later command.
    let response = client.send(&rest::me()).await.map_err(|e| e.to_string())?;
    let user = rest::parse_user(&response).map_err(|e| format!("the token was rejected: {e}"))?;

    session::save(&session::Session {
        server: client.base().to_string(),
        token,
        user_id: user.id.to_string(),
        username: user.username.clone(),
    })?;
    println!("stored a token for {} on {}", user.username, client.base());
    Ok(())
}

async fn whoami() -> Result<(), String> {
    let (client, saved) = connect()?;
    let response = client.send(&rest::me()).await.map_err(|e| e.to_string())?;
    let user = rest::parse_user(&response).map_err(|e| e.to_string())?;
    println!("{} ({}) on {}", user.username, user.id, saved.server);
    if !user.full_name().is_empty() {
        println!("name: {}", user.full_name());
    }
    println!("roles: {}", user.roles);
    Ok(())
}

fn connect() -> Result<(Client, session::Session), String> {
    let saved = session::load()?;
    let client = Client::new(&saved.server)
        .map_err(|e| e.to_string())?
        .with_token(saved.token.clone());
    Ok((client, saved))
}

// ---------------------------------------------------------------- sync

/// Load everything the sidebar needs and print it.
///
/// This is the whole initial-load path the GUI will use, in the order that
/// matters: teams, then channels and my membership of them, which is what
/// makes the unread counts correct before any post is fetched.
async fn sync(args: &[String]) -> Result<(), String> {
    let (client, saved) = connect()?;
    let mut store = Store::new(UserId::new(saved.user_id.clone()), Threading::Flat);

    // Collapsed reply threads change which counters the unread arithmetic
    // reads, and the server default has changed across versions, so it must be
    // asked rather than assumed.
    let config = fetch(&client, &rest::client_config(), rest::parse_client_config).await?;
    if let Some(mode) = config.get("CollapsedThreads") {
        if mode == "always_on" || mode == "default_on" {
            store.threading = Threading::Collapsed;
        }
        println!("server CollapsedThreads={mode} -> {:?}", store.threading);
    }

    let teams = fetch(&client, &rest::my_teams(), rest::parse_teams).await?;
    println!("{} team(s)", teams.len());

    for team in &teams {
        let channels = fetch(&client, &rest::my_channels(&team.id), rest::parse_channels).await?;
        let members = fetch(
            &client,
            &rest::my_channel_members(&team.id),
            rest::parse_channel_members,
        )
        .await?;
        store.apply_channels(channels);
        store.apply_members(members);
    }
    store.apply_teams(teams);

    // DM labels come from the roster, so fetch the participants of any DM.
    let dm_ids: Vec<String> = store
        .channels
        .values()
        .filter(|c| c.kind == mm_types::ChannelType::Direct)
        .flat_map(|c| c.name.split("__").map(str::to_owned).collect::<Vec<_>>())
        .filter(|id| *id != saved.user_id)
        .collect();
    if !dm_ids.is_empty() {
        let users = fetch(&client, &rest::users_by_ids(&dm_ids), rest::parse_users).await?;
        store.apply_users(users);
    }

    let rows = store.sidebar();
    println!(
        "\n{} channel(s), {} unread, {} mention(s)\n",
        rows.len(),
        rows.iter().filter(|r| r.is_unread()).count(),
        store.total_mentions()
    );
    for row in &rows {
        let marker = if row.is_unread() { '*' } else { ' ' };
        let mentions = if row.unread.mentions > 0 {
            format!(" [{}]", row.unread.mentions)
        } else {
            String::new()
        };
        let muted = if row.muted { " (muted)" } else { "" };
        println!(
            "{marker} {:>4}{mentions:<5} {}{muted}",
            row.unread.messages, row.display_name
        );
    }

    if let Some(name) = flag(args, "--channel") {
        show_channel(&client, &mut store, &name).await?;
    }
    Ok(())
}

/// Post a message, so the create-post path is exercisable without the window.
async fn send(args: &[String]) -> Result<(), String> {
    refuse_if_read_only("send")?;
    let name = require(args, "--channel")?;
    let text = require(args, "--text")?;
    let (client, saved) = connect()?;

    let channel = resolve_channel(&client, &name).await?;

    // The pending id is what lets an optimistic post on screen be matched to
    // the `posted` event that confirms it, instead of appearing twice.
    let pending = format!("mmrs-{}-{}", saved.user_id, now_millis());
    let request = rest::create_post(&channel, &text, None, &pending, &[]);
    let post = fetch(&client, &request, rest::parse_post).await?;
    println!("posted {} to {name} at {}", post.id, post.create_at);
    Ok(())
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

async fn show_channel(client: &Client, store: &mut Store, name: &str) -> Result<(), String> {
    let found = store
        .channels
        .values()
        .find(|c| c.name == name || store.channel_label(c) == name)
        .map(|c| c.id.clone())
        .ok_or_else(|| format!("no channel called {name:?}"))?;

    // Ask the way the store is set up to read: with collapsed threads the
    // server must be told, or a page of sixty posts is mostly replies the
    // view then hides.
    let page = fetch_posts(client, &found, store.threading == Threading::Collapsed).await?;
    store.apply_post_list(&found, &page, PageAnchor::Newest);

    println!("\n--- {name} ---");
    for post in store.channel_posts(&found) {
        let author = store
            .users
            .get(&post.user_id)
            .map_or_else(|| post.user_id.to_string(), |u| u.display_name().to_owned());
        let edited = if post.is_edited() { " (edited)" } else { "" };
        println!("[{}] {author}: {}{edited}", post.create_at, post.message);
    }
    Ok(())
}

async fn fetch_posts(
    client: &Client,
    channel: &ChannelId,
    collapsed: bool,
) -> Result<mm_types::PostList, String> {
    fetch(
        client,
        &rest::posts_for_channel(channel, &PostQuery::first(), collapsed),
        rest::parse_post_list,
    )
    .await
}

/// Send a request and parse it, turning both failure kinds into one message.
async fn fetch<T>(
    client: &Client,
    request: &mm_api::HttpRequest,
    parse: impl Fn(&mm_api::HttpResponse) -> Result<T, ApiError>,
) -> Result<T, String> {
    let response = client
        .send(request)
        .await
        .map_err(|e| format!("{}: {e}", request.route_key()))?;
    parse(&response).map_err(|e| format!("{}: {e}", request.route_key()))
}

// ---------------------------------------------------------------- watch

/// Connect the WebSocket and print what arrives, applying it to the store.
///
/// This is the cheapest way to see that the protocol layer and the reducers
/// agree with a live server: send a message from another client and watch the
/// unread count move here.
async fn watch(args: &[String]) -> Result<(), String> {
    let (client, saved) = connect()?;
    let seconds: u64 = flag(args, "--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let mut protocol = WsProtocol::new();
    let url = client
        .websocket_url(&protocol.reconnect_query(None))
        .map_err(|e| e.to_string())?;
    println!("connecting to {url}");

    let mut socket = mm_http::WsConn::connect(&url, &saved.token)
        .await
        .map_err(|e| e.to_string())?;
    let mut store = Store::new(UserId::new(saved.user_id), Threading::Flat);

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(seconds);
    println!("listening for {seconds}s — send yourself a message to see it arrive\n");

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            println!("\n(time is up)");
            return Ok(());
        }
        let frame = match tokio::time::timeout(remaining, socket.next_text()).await {
            Err(_) => continue,
            Ok(None) => {
                println!("the server closed the connection");
                return Ok(());
            }
            Ok(Some(Err(e))) => return Err(e.to_string()),
            Ok(Some(Ok(text))) => text,
        };

        for outcome in protocol.on_frame(&frame) {
            match outcome {
                WsOutcome::Hello {
                    server_version,
                    lost_events,
                    ..
                } => {
                    println!("hello from server {server_version}");
                    if lost_events {
                        println!("  ! events were lost; a REST resync would be required here");
                    }
                }
                WsOutcome::SequenceGap { expected, got } => {
                    println!("! sequence gap: expected {expected}, got {got} — resync required");
                }
                WsOutcome::Event(event) => {
                    let changes = store.apply_ws(&event);
                    println!("{}", describe(&event));
                    for change in changes {
                        println!("  -> {change:?}");
                    }
                }
                WsOutcome::Reply { client_seq, ok, .. } => {
                    println!("reply to #{client_seq}: ok={ok}");
                }
                WsOutcome::Undecodable { detail } => println!("? undecodable frame: {detail}"),
            }
        }
    }
}

fn describe(event: &mm_api::WsEvent) -> String {
    use mm_api::WsEvent as E;
    match event {
        E::Posted {
            post, sender_name, ..
        } => {
            format!(
                "posted by {sender_name} in {}: {}",
                post.channel_id, post.message
            )
        }
        E::PostEdited(p) => format!("edited {}", p.id),
        E::PostDeleted(p) => format!("deleted {}", p.id),
        E::ChannelMemberUpdated(m) => format!(
            "read state for {}: {} unread-seen, {} mentions",
            m.channel_id, m.msg_count, m.mention_count
        ),
        E::MultipleChannelsViewed { channel_times } => {
            format!("{} channel(s) marked viewed", channel_times.len())
        }
        E::ChannelViewed { channel_id } => format!("{channel_id} marked viewed (legacy event)"),
        E::Typing { user_id, .. } => format!("{user_id} is typing"),
        E::StatusChange { user_id, status } => format!("{user_id} is now {status}"),
        E::UserUpdated(u) => format!("user {} updated", u.username),
        E::ReactionAdded(r) => format!("{} reacted :{}: to {}", r.user_id, r.emoji_name, r.post_id),
        E::ReactionRemoved(r) => {
            format!(
                "{} took back :{}: on {}",
                r.user_id, r.emoji_name, r.post_id
            )
        }
        E::EmojiAdded(e) => format!("custom emoji :{}: added", e.name),
        E::UserAddedToChannel {
            channel_id,
            user_id,
        } => format!("{user_id} joined {channel_id}"),
        E::UserRemovedFromChannel {
            channel_id,
            user_id,
        } => format!("{user_id} left {channel_id}"),
        E::ThreadUpdated(t) => format!(
            "thread {} now has {} replies, {} unread",
            t.id, t.reply_count, t.unread_replies
        ),
        E::ThreadReadChanged {
            thread_id,
            unread_replies,
            ..
        } => format!("thread {thread_id} read, {unread_replies} unread left"),
        E::PreferencesChanged(prefs) => format!("{} preference(s) changed", prefs.len()),
        E::PreferencesDeleted(prefs) => format!("{} preference(s) deleted", prefs.len()),
        E::Unknown { event } => format!("(unhandled event {event})"),
    }
}

#[cfg(test)]
mod tests {
    use super::split_args;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    /// The regression this exists for: `mattermost-rs` with no arguments
    /// panicked on `&args[1..]` before reaching anything that had been tested.
    /// It is the most common way the binary is started.
    #[test]
    fn no_arguments_opens_the_window() {
        let (command, rest) = split_args(&[]);
        assert_eq!(command, "gui");
        assert!(rest.is_empty());
    }

    #[test]
    fn a_bare_command_has_no_arguments() {
        let args = owned(&["sync"]);
        let (command, rest) = split_args(&args);
        assert_eq!(command, "sync");
        assert!(rest.is_empty());
    }

    #[test]
    fn a_command_keeps_everything_after_it() {
        let args = owned(&["send", "--channel", "town-square", "--text", "hi"]);
        let (command, rest) = split_args(&args);
        assert_eq!(command, "send");
        assert_eq!(
            rest,
            &owned(&["--channel", "town-square", "--text", "hi"])[..]
        );
    }

    /// A flag in the command position is still the command, so `--version`
    /// and `--help` reach their handlers rather than being treated as an
    /// argument to something.
    #[test]
    fn a_leading_flag_is_the_command() {
        for flag in ["--version", "--help", "-h", "-V"] {
            let args = owned(&[flag]);
            let (command, rest) = split_args(&args);
            assert_eq!(command, flag);
            assert!(rest.is_empty());
        }
    }
}
