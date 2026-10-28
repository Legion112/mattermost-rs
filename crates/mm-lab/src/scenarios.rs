//! What the UI tests check, and the runner that drives them.
//!
//! Each scenario presses the same widgets a person would, then asks the
//! *server* whether it actually happened. Both halves matter: the window
//! agreeing with itself is not the same as the window being right, and a
//! button that redraws the screen without reaching the server would pass a
//! test that only read the widget tree.
//!
//! The runner starts the client itself, with a stored session thrown away
//! first, so signing in is the first thing tested and every run begins from
//! the same place. A failure writes a screenshot beside its message.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::input::key;
use crate::seed::Admin;
use crate::ui::{Find, Node, Screen};
use crate::{Error, Result};

/// Every scenario, in the order they run.
///
/// Order is part of the test: signing in has to come first, and
/// `leaves-a-channel` is last because it is the only one that changes what
/// the sidebar holds.
pub const SCENARIOS: &[&str] = &[
    "signs-in",
    "sets-a-custom-status",
    "goes-quiet-for-an-hour",
    "edits-the-profile",
    "shows-a-post-saved-elsewhere",
    "shows-a-thread-reply-count",
    "shows-a-new-reply-in-threads",
    "shows-who-is-typing",
    "follows-edits-reactions-and-deletions",
    "shows-an-unread-and-clears-it",
    "shows-a-new-direct-message",
    "mutes-and-unmutes",
    "sets-how-the-channel-notifies",
    "keeps-a-draft-on-the-server",
    "sends-a-draft-with-its-file",
    "sends-a-message",
    "edits-with-the-up-key",
    "reacts-with-the-keyboard",
    "searches-messages",
    "opens-a-card-from-a-mention",
    "pins-from-the-hover-toolbar",
    "saves-from-the-menu",
    "marks-unread-from-the-menu",
    "replies-in-the-thread-box",
    "jumps-with-the-switcher",
    "adds-and-removes-somebody",
    "refuses-to-leave-town-square",
    "edits-the-channel-header",
    "archives-from-the-menu",
    "makes-a-channel-private",
    "opens-an-archived-channel",
    "leaves-a-channel",
    "joins-from-the-switcher",
    "tidies-the-lab",
];

/// The channel the seed makes for exactly this, so that joining and leaving
/// costs nothing.
const PRACTICE: &str = "leaving-practice";
const PRACTICE_LABEL: &str = "Leaving Practice";

pub async fn run(only: Option<&str>, shots: &Path, keep_data: bool, guest: bool) -> Result<()> {
    // A fresh server unless told otherwise, and this is not fussiness.
    // Every run of `adds-and-removes-somebody` leaves two system notices in
    // Engineering -- "linus added", "linus removed" -- and after enough runs
    // they push the seeded thread off the channel's first page, at which
    // point `shows-a-thread-reply-count` starts failing for a reason that
    // has nothing to do with reply counts. A test whose result depends on
    // how many times it has been run before is not a test.
    // A guest is inside the lab's own network namespace with no Docker to
    // call: the server is already up and seeded by whoever started this
    // container, and `localhost:8065` is it.
    if guest {
        crate::stack::wait_ready().await?;
    } else {
        if !keep_data {
            crate::stack::down(false)?;
        }
        crate::stack::up().await?;
        crate::seed::seed().await?;
    }
    let admin = Admin::connect().await?;
    // Only matters with --keep-data: put back what a previous run left
    // changed, so this one starts where that one did.
    admin.rejoin(PRACTICE).await?;

    let home = lab_home();
    // Thrown away rather than reused: signing in is the first scenario, and
    // a run that began already signed in would never test it.
    let _ = std::fs::remove_dir_all(&home);
    let mut client = Client::start(&home)?;
    let screen = Screen::attach_waiting("mmrs.lab").await?;

    let mut failed = Vec::new();
    for name in SCENARIOS {
        // Signing in is setup, not an optional scenario: `--only` on any
        // other one still has to get past the login screen.
        if only.is_some_and(|wanted| !wanted.split(',').any(|w| w == *name)) && *name != "signs-in"
        {
            continue;
        }
        let started = std::time::Instant::now();
        let outcome = one(name, &screen, &admin).await;
        let took = started.elapsed().as_millis();
        match outcome {
            Ok(()) => println!("  ok     {name} ({took}ms)"),
            Err(Error(why)) => {
                let shot = match crate::shot::capture(shots, name, screen.window_area().await) {
                    Ok(path) => path.display().to_string(),
                    Err(e) => format!("(no screenshot: {e})"),
                };
                println!("  FAILED {name} ({took}ms)\n{why}\n  screenshot: {shot}");
                failed.push(*name);
            }
        }
    }

    client.stop();
    if failed.is_empty() {
        println!("ui: every scenario passed");
        return Ok(());
    }
    Err(Error(format!(
        "ui: {} scenario(s) failed: {failed:?}",
        failed.len()
    )))
}

async fn one(name: &str, screen: &Screen, admin: &Admin) -> Result<()> {
    match name {
        "signs-in" => signs_in(screen).await,
        "sets-a-custom-status" => custom_status(screen, admin).await,
        "goes-quiet-for-an-hour" => quiet_for_an_hour(screen, admin).await,
        "edits-the-profile" => edits_profile(screen, admin).await,
        "shows-a-post-saved-elsewhere" => saved_elsewhere(screen, admin).await,
        "shows-a-thread-reply-count" => reply_count(screen).await,
        "shows-a-new-reply-in-threads" => new_reply_in_threads(screen, admin).await,
        "shows-who-is-typing" => who_is_typing(screen, admin).await,
        "follows-edits-reactions-and-deletions" => live_post_changes(screen, admin).await,
        "shows-an-unread-and-clears-it" => unread_and_clears(screen, admin).await,
        "shows-a-new-direct-message" => new_direct_message(screen, admin).await,
        "mutes-and-unmutes" => mutes(screen, admin).await,
        "sets-how-the-channel-notifies" => notify_level(screen, admin).await,
        "keeps-a-draft-on-the-server" => draft_on_server(screen, admin).await,
        "sends-a-draft-with-its-file" => draft_with_file(screen, admin).await,
        "sends-a-message" => sends_a_message(screen, admin).await,
        "edits-with-the-up-key" => edits_with_up(screen, admin).await,
        "reacts-with-the-keyboard" => reacts_by_key(screen, admin).await,
        "searches-messages" => searches(screen).await,
        "opens-a-card-from-a-mention" => card_from_mention(screen).await,
        "pins-from-the-hover-toolbar" => pins_from_toolbar(screen, admin).await,
        "saves-from-the-menu" => saves_from_menu(screen, admin).await,
        "marks-unread-from-the-menu" => marks_unread_from_menu(screen, admin).await,
        "replies-in-the-thread-box" => replies_in_thread(screen, admin).await,
        "jumps-with-the-switcher" => jumps_with_switcher(screen).await,
        "adds-and-removes-somebody" => add_and_remove(screen, admin).await,
        "refuses-to-leave-town-square" => town_square(screen).await,
        "leaves-a-channel" => leaves(screen, admin).await,
        "opens-an-archived-channel" => archived_channel(screen, admin).await,
        "edits-the-channel-header" => edits_header(screen, admin).await,
        "archives-from-the-menu" => archives_from_menu(screen, admin).await,
        "makes-a-channel-private" => makes_private(screen, admin).await,
        "joins-from-the-switcher" => joins_from_switcher(screen, admin).await,
        "tidies-the-lab" => tidies(admin).await,
        other => Err(Error(format!("no scenario called {other:?}"))),
    }
}

/// Typing a server, a name and a password into the form, and getting a
/// sidebar for it.
async fn signs_in(screen: &Screen) -> Result<()> {
    let form = screen
        .until("the sign-in form is up", |tree| {
            tree.has("button", "Log in")
        })
        .await?;
    screen
        .type_into(form.find("text box", "Server")?, crate::stack::URL)
        .await?;
    screen
        .type_into(
            form.find("text box", "Email or username")?,
            crate::seed::ADMIN,
        )
        .await?;
    screen
        .type_into(form.find("text box", "Password")?, crate::seed::PASSWORD)
        .await?;
    screen.press("Log in").await?;
    screen
        .until("the sidebar is drawn", |tree| tree.has("label", "CHANNELS"))
        .await
        .map(|_| ())
}

/// Setting a custom status from the status menu reaches the server, and
/// clearing it from the same menu takes it back. The window is checked
/// only for having the dialog; what matters is the server's copy of the
/// user, which is the one everybody else reads.
async fn custom_status(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.press("Set your status").await?;
    screen.press("Set a custom status…").await?;
    let form = screen
        .until("the status dialog is up", |tree| {
            tree.has("text box", "What are you up to?")
        })
        .await?;
    screen
        .type_into(form.find("text box", "What are you up to?")?, "In the lab")
        .await?;
    screen.press("Set status").await?;
    server_holds(admin, Some("In the lab")).await?;
    // And the reader's own shows beside their dot, as the default emoji
    // the dialog started on, once the server has told everybody.
    screen
        .until("the reader's own status shows beside the dot", |tree| {
            tree.has("label", "💬")
        })
        .await?;

    screen.press("Set your status").await?;
    screen
        .until("the status menu shows what was set", |tree| {
            tree.has("label", "In the lab") && tree.has("label", "Clear custom status")
        })
        .await?;
    screen.press("Clear custom status").await?;
    server_holds(admin, None).await?;

    // And somebody else's is drawn beside their name on every message
    // of theirs, arriving over the socket without the rows being
    // rebuilt -- and leaving the same way when it is cleared. Ada, not
    // grace: grace speaks only in replies, which are in the thread.
    crate::seed::set_custom_status("ada", "palm_tree", "On a beach").await?;
    screen.open_channel("Engineering").await?;
    screen
        .until("ada's status shows beside her name", |tree| {
            tree.has("label", "🌴")
        })
        .await?;
    crate::seed::clear_custom_status("ada").await?;
    screen
        .until("ada's status is gone from beside her name", |tree| {
            !tree.has("label", "🌴")
        })
        .await
        .map(|_| ())
}

/// Wait, briefly, for the server's copy of the admin's custom status to
/// say what it should.
async fn server_holds(admin: &Admin, expected: Option<&str>) -> Result<()> {
    let mut last = None;
    for _ in 0..25 {
        last = admin.my_custom_status().await?;
        if last.as_deref() == expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server holds custom status {last:?}, expected {expected:?}"
    )))
}

/// A post saved from another client of this account shows up in the
/// saved view without a sync, and goes when it is unsaved there.
///
/// Saving is a preference on the wire, announced as
/// `preferences_changed`, which this client used not to decode -- so
/// the view was a photograph of the last sync. Design is opened first
/// so the message can only be on screen because the saved list has it.
async fn saved_elsewhere(screen: &Screen, admin: &Admin) -> Result<()> {
    const ROOT: &str = "Morning. The nightly run";
    screen.open_channel("Design").await?;
    screen.press("Saved messages").await?;
    admin.save_post("engineering", ROOT, true).await?;
    screen
        .until("the saved post appears in the saved view", |tree| {
            tree.has("label", ROOT)
        })
        .await?;
    admin.save_post("engineering", ROOT, false).await?;
    screen
        .until("the unsaved post goes from the saved view", |tree| {
            !tree.has("label", ROOT)
        })
        .await
        .map(|_| ())
}

/// Somebody typing in the open channel is named under the messages.
async fn who_is_typing(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    admin.typing_as("ada", "engineering").await?;
    screen
        .until("the typing line names ada", |tree| {
            tree.has("label", "ada is typing…")
        })
        .await
        .map(|_| ())
}

/// A colleague's message arrives, gets a reaction, is edited and is
/// deleted, each over the socket, and the window follows every step.
async fn live_post_changes(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    let said = "A fresh message, for reacting to.";
    let post = admin.say_as("ada", "engineering", said).await?;
    screen
        .until("the new message is drawn", |tree| tree.has("label", said))
        .await?;
    admin.react_as("ada", &post, "thumbsup").await?;
    screen
        .until("the reaction chip appears", |tree| {
            tree.has("button", "👍 1")
        })
        .await?;
    let edited = "A fresh message, edited.";
    admin.edit_as("ada", &post, edited).await?;
    screen
        .until("the edit is drawn", |tree| {
            tree.has("label", edited) && !tree.has("label", said)
        })
        .await?;
    admin.delete_as("ada", &post).await?;
    screen
        .until("the deleted message is gone", |tree| {
            !tree.has("label", edited)
        })
        .await
        .map(|_| ())
}

/// The label right after a sidebar row's name, which is where its badge
/// or unread dot sits. The *last* label with that name: the title bar
/// says the open channel's name too, and comes first.
fn badge_after<'a>(tree: &'a [Node], row: &str) -> Option<&'a str> {
    let at = tree
        .iter()
        .rposition(|n| n.role == "label" && n.name == row)?;
    let next = tree.get(at + 1)?;
    (next.role == "label").then_some(next.name.as_str())
}

/// A mention in a channel that is not open puts a badge on its row,
/// and opening the channel clears it on the server too.
async fn unread_and_clears(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    admin
        .say_as("ada", "design", "@legion something new in Design.")
        .await?;
    screen
        .until("the Design row carries a mention badge", |tree| {
            badge_after(tree, "Design") == Some("1")
        })
        .await?;
    screen.open_channel("Design").await?;
    screen
        .until("the badge is gone", |tree| {
            !matches!(badge_after(tree, "Design"), Some("1" | "●"))
        })
        .await?;
    let mut unread = -1;
    for _ in 0..25 {
        unread = admin.my_unread("design").await?;
        if unread == 0 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server still counts {unread} unread in Design after it was opened"
    )))
}

/// A first message from somebody with no conversation yet makes a
/// row in the sidebar, and the row opens on the message.
async fn new_direct_message(screen: &Screen, admin: &Admin) -> Result<()> {
    // Design, not Engineering: grace speaks in Engineering's thread, and
    // her name must only be on screen because the sidebar has it.
    screen.open_channel("Design").await?;
    let said = "Hello from grace, out of nowhere.";
    admin.dm_from("grace", said).await?;
    screen
        .until("grace appears in the sidebar", |tree| {
            tree.has("label", "grace")
        })
        .await?;
    screen.select_row("grace").await?;
    screen
        .until("her message is drawn", |tree| tree.has("label", said))
        .await
        .map(|_| ())
}

/// Wait for the server's copy of a membership prop to say something.
async fn membership_says(admin: &Admin, channel: &str, prop: &str, expected: &str) -> Result<()> {
    let mut last = String::new();
    for _ in 0..25 {
        let membership = admin.my_membership(channel).await?;
        membership["notify_props"][prop]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut last);
        if last == expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server has {prop}={last:?} on {channel}, expected {expected:?}"
    )))
}

/// The mute switch in the channel menu reaches the server both ways.
async fn mutes(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen.press("Engineering").await?;
    screen.press("Mute").await?;
    membership_says(admin, "engineering", "mark_unread", "mention").await?;
    screen.press("Mute").await?;
    membership_says(admin, "engineering", "mark_unread", "all").await?;
    screen.close_menu("Engineering").await
}

/// The row of notify buttons in the channel menu reaches the server.
async fn notify_level(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen.press("Engineering").await?;
    screen.press("Mentions").await?;
    membership_says(admin, "engineering", "desktop", "mention").await?;
    screen.press("Default").await?;
    membership_says(admin, "engineering", "desktop", "default").await?;
    screen.close_menu("Engineering").await
}

/// Wait for the server's drafts to hold, or not hold, a text.
async fn drafts_hold(admin: &Admin, text: &str, expected: bool) -> Result<()> {
    let mut last = Vec::new();
    for _ in 0..40 {
        last = admin.my_drafts().await?;
        if last.iter().any(|d| d == text) == expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server's drafts are {last:?}; expected {text:?} to be held: {expected}"
    )))
}

/// Typing in the box writes a draft to the server a moment later, and
/// emptying the box takes it back.
async fn draft_on_server(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    let tree = screen
        .until("the composer is on screen", |tree| {
            tree.has("text box", "Write a message…")
        })
        .await?;
    let composer = tree.find("text box", "Write a message…")?;
    let half = "Half a thought, left in the box";
    screen.type_into(composer, half).await?;
    drafts_hold(admin, half, true).await?;
    // And the row says so, with a pencil after the name.
    screen
        .until("the row shows a pencil for the draft", |tree| {
            badge_after(tree, "Engineering") == Some("✎")
        })
        .await?;
    screen.type_into(composer, "").await?;
    drafts_hold(admin, half, false).await?;
    screen
        .until("the pencil goes with the draft", |tree| {
            badge_after(tree, "Engineering") != Some("✎")
        })
        .await
        .map(|_| ())
}

/// Wait for the server to agree a channel says something, or not.
async fn server_hears(admin: &Admin, channel: &str, text: &str, expected: bool) -> Result<()> {
    for _ in 0..25 {
        if admin.channel_says(channel, text).await? == expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server {} {text:?} in {channel}",
        if expected { "never heard" } else { "still has" }
    )))
}

/// The composer, clicked into so it has the keyboard.
async fn focused_composer(screen: &Screen) -> Result<Node> {
    let tree = screen
        .until("the composer is on screen", |tree| {
            tree.has("text box", "Write a message…")
        })
        .await?;
    let composer = tree.find("text box", "Write a message…")?.clone();
    screen.click_into(&composer).await?;
    Ok(composer)
}

/// What the whole thing is for: a message typed and sent with Return,
/// and the server holding it.
async fn sends_a_message(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    let composer = focused_composer(screen).await?;
    let said = format!("Sent from the lab at {}", crate::seed::stamp());
    screen.type_into(&composer, &said).await?;
    screen.press_key(key::RETURN, &[])?;
    screen
        .until("the message is drawn", |tree| tree.has("label", &said))
        .await?;
    server_hears(admin, "engineering", &said, true).await
}

/// Up in an empty box opens the editor on the reader's last message;
/// saving changes it on the server.
async fn edits_with_up(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    let composer = focused_composer(screen).await?;
    screen.type_into(&composer, "").await?;
    screen.press_key(key::UP, &[])?;
    let editor = screen
        .until("the editor is up", |tree| {
            tree.has("text box", "Edit message")
        })
        .await?;
    let edited = format!("Edited from the lab at {}", crate::seed::stamp());
    screen
        .type_into(editor.find("text box", "Edit message")?, &edited)
        .await?;
    screen.press("Save").await?;
    screen
        .until("the edit is drawn", |tree| tree.has("label", &edited))
        .await?;
    server_hears(admin, "engineering", &edited, true).await
}

/// Ctrl+Shift+\ opens the picker on the last message; picking one
/// puts the reaction on the server.
async fn reacts_by_key(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    focused_composer(screen).await?;
    screen.press_key(key::BACKSLASH, &[key::CONTROL, key::SHIFT])?;
    // The table is long; the search box at its top narrows it to one.
    let picker = screen
        .until("the picker is open", |tree| {
            tree.has("search box", "Search emoji…")
        })
        .await?;
    let search = picker.find("search box", "Search emoji…")?.clone();
    screen.type_into(&search, "tada").await?;
    // The grid fills a moment after the search changes; pressing before
    // it has found an older message's "🎉 1" chip instead.
    screen
        .until("the picker offers tada", |tree| {
            tree.iter().any(|n| n.role == "button" && n.name == "🎉")
        })
        .await?;
    screen.press("🎉").await?;
    let mut names = Vec::new();
    for _ in 0..25 {
        let (_, found) = admin.newest_with_reactions("engineering").await?;
        if found.iter().any(|n| n == "tada") {
            return Ok(());
        }
        names = found;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the newest message carries reactions {names:?}, not the tada the picker sent"
    )))
}

/// A search typed into the box and sent with Return finds the seeded
/// thread root.
async fn searches(screen: &Screen) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen.press("Search messages").await?;
    let page = screen
        .until("the search box is up", |tree| {
            tree.has("search box", "Search messages…")
        })
        .await?;
    let entry = page.find("search box", "Search messages…")?.clone();
    screen.click_into(&entry).await?;
    screen.type_into(&entry, "nightly").await?;
    screen.press_key(key::RETURN, &[])?;
    screen
        .until("the thread root is among the results", |tree| {
            tree.has("label", "Morning. The nightly run")
        })
        .await?;
    // Back to a channel, so the next scenario starts where it expects.
    screen.open_channel("Engineering").await
}

/// Clicking an @name inside a message opens that person's card.
async fn card_from_mention(screen: &Screen) -> Result<()> {
    // Design holds "@legion something new in Design." from the unread
    // scenario, as a message rather than a thread reply.
    screen.open_channel("Design").await?;
    // The message is a label, which cannot place a word for a click;
    // the name is its first word, so the click goes to its start.
    let body = screen.text_holding("@legion something new").await?;
    screen.click_start_of(&body).await?;
    screen
        .until("the card names the admin", |tree| {
            tree.has("label", "Legion Admin") && tree.has("label", "@legion")
        })
        .await?;
    screen.press_key(key::ESCAPE, &[])?;
    screen
        .until("the card is gone", |tree| {
            !tree.has("label", "Legion Admin")
        })
        .await
        .map(|_| ())
}

/// A timed do-not-disturb from the status menu reaches the server with
/// its end, and going back online takes it off.
async fn quiet_for_an_hour(screen: &Screen, admin: &Admin) -> Result<()> {
    let asked = i64::try_from(crate::seed::stamp() / 1000).unwrap_or(0);
    screen.press("Set your status").await?;
    screen.press("Do not disturb for 1 hour").await?;
    let mut last = (String::new(), 0);
    for _ in 0..25 {
        last = admin.my_status().await?;
        if last.0 == "dnd" && (last.1 - asked - 3600).abs() <= 120 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if last.0 != "dnd" || (last.1 - asked - 3600).abs() > 120 {
        return Err(Error(format!(
            "the server holds {last:?}; expected dnd ending about an hour after {asked}"
        )));
    }
    screen.press("Set your status").await?;
    screen.press("Online").await?;
    for _ in 0..25 {
        last = admin.my_status().await?;
        if last.0 == "online" {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server still holds {last:?} after Online"
    )))
}

/// Wait for the server's word on whether a post is pinned.
async fn pinned_says(admin: &Admin, post: &str, expected: bool) -> Result<()> {
    for _ in 0..25 {
        if admin.is_pinned(post).await? == expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server does not hold the post pinned = {expected}"
    )))
}

/// Resting the pointer on a message brings its toolbar up, and the
/// overflow menu's Pin and Unpin reach the server.
async fn pins_from_toolbar(screen: &Screen, admin: &Admin) -> Result<()> {
    const ROOT: &str = "Morning. The nightly run";
    screen.open_channel("Engineering").await?;
    let post = admin.post_id_of("engineering", ROOT).await?;
    for (entry, expected) in [("Pin to channel", true), ("Unpin from channel", false)] {
        let body = screen.text_holding(ROOT).await?;
        screen.scroll_into_view(&body).await?;
        screen.hover(&body).await?;
        screen.click_button_near("More actions", &body).await?;
        screen.press(entry).await?;
        pinned_says(admin, &post, expected).await?;
    }
    Ok(())
}

/// A reply typed into the thread box and sent with Return lands under
/// the root on the server.
async fn replies_in_thread(screen: &Screen, admin: &Admin) -> Result<()> {
    const ROOT: &str = "Morning. The nightly run";
    screen.open_channel("Engineering").await?;
    let before = admin.replies_to("engineering", ROOT).await?;
    screen.press(" replies").await?;
    let pane = screen
        .until("the thread box is up", |tree| {
            tree.has("text box", "Reply…")
        })
        .await?;
    let reply_box = pane.find("text box", "Reply…")?.clone();
    screen.click_into(&reply_box).await?;
    let said = format!("Replied from the lab at {}", crate::seed::stamp());
    screen.type_into(&reply_box, &said).await?;
    screen.press_key(key::RETURN, &[])?;
    let mut after = before;
    for _ in 0..25 {
        after = admin.replies_to("engineering", ROOT).await?;
        if after == before + 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    screen.press_key(key::ESCAPE, &[])?;
    if after != before + 1 {
        return Err(Error(format!(
            "the thread had {before} replies and has {after}, not one more"
        )));
    }
    Ok(())
}

/// Ctrl+K, a few letters and Return open the channel named.
async fn jumps_with_switcher(screen: &Screen) -> Result<()> {
    screen.open_channel("Engineering").await?;
    focused_composer(screen).await?;
    screen.press_key(0x6b, &[key::CONTROL])?;
    let finder = screen
        .until("the switcher is up", |tree| {
            tree.has("search box", "Find channel…")
        })
        .await?;
    let entry = finder.find("search box", "Find channel…")?.clone();
    screen.click_into(&entry).await?;
    screen.type_into(&entry, "Design").await?;
    // The list fills a moment after the text changes; Return before
    // that acts on whatever was selected before.
    screen
        .until("the switcher offers Design", |tree| {
            tree.iter()
                .filter(|n| n.role == "label" && n.name == "Design")
                .count()
                >= 2
        })
        .await?;
    // What the finder holds at the moment of Return, for the log: on
    // one fresh lab Return opened a conversation it should not have.
    let before = screen.tree().await?;
    let from = before
        .iter()
        .position(|n| n.role == "search box" && n.name == "Find channel…")
        .unwrap_or(0);
    let rows: Vec<String> = before[from..]
        .iter()
        .filter(|n| !n.name.is_empty())
        .map(|n| format!("{} {:?}", n.role, n.name))
        .collect();
    eprintln!("  the finder holds {rows:?}");
    screen.press_key(key::RETURN, &[])?;
    screen
        .until("Design is open", |tree| tree.has("button", "Design, "))
        .await
        .map(|_| ())
}

/// Last of all, the lab's own traces go, so a kept lab is next run's
/// seed and not a pile of every run so far. Not a test of the window.
async fn tidies(admin: &Admin) -> Result<()> {
    for channel in ["engineering", "design"] {
        admin.sweep_traces(channel).await?;
    }
    Ok(())
}

/// After leaving, the switcher still finds the channel among the ones
/// the reader is not in, and Return joins it: the server agrees.
async fn joins_from_switcher(screen: &Screen, admin: &Admin) -> Result<()> {
    if admin.am_in(PRACTICE).await? {
        return Err(Error(format!(
            "still in {PRACTICE}, so joining it proves nothing"
        )));
    }
    focused_composer(screen).await?;
    screen.press_key(0x6b, &[key::CONTROL])?;
    let finder = screen
        .until("the switcher is up", |tree| {
            tree.has("search box", "Find channel…")
        })
        .await?;
    let entry = finder.find("search box", "Find channel…")?.clone();
    screen.click_into(&entry).await?;
    screen.type_into(&entry, "Leaving").await?;
    screen
        .until("the switcher offers the channel left", |tree| {
            tree.has("label", PRACTICE_LABEL)
        })
        .await?;
    screen.press_key(key::RETURN, &[])?;
    screen
        .until("the channel is open again", |tree| {
            tree.has("button", &format!("{PRACTICE_LABEL}, "))
        })
        .await?;
    for _ in 0..25 {
        if admin.am_in(PRACTICE).await? {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server does not have us back in {PRACTICE}"
    )))
}

/// A draft written elsewhere that names a file comes back with the file
/// as a chip, and sending it sends the file: before this the text came
/// back and the file was quietly dropped.
async fn draft_with_file(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Design").await?;
    let file = admin
        .upload_note("engineering", "lab-note.txt", b"hello from the lab")
        .await?;
    let text = format!("Draft with a file at {}", crate::seed::stamp());
    admin.draft_with_file("engineering", &text, &file).await?;
    // Drafts arrive with a sync; the reload button is one.
    screen.press("Reload channels").await?;
    screen.open_channel("Engineering").await?;
    let tree = screen
        .until("the draft's file is a chip in the tray", |tree| {
            tree.has("label", "lab-note.txt")
        })
        .await?;
    let composer = tree.find("text box", "Write a message…")?.clone();
    let held = screen.text_of(&composer).await?;
    if held != text {
        return Err(Error(format!(
            "the box holds {held:?}, not the draft {text:?}"
        )));
    }
    screen.click_into(&composer).await?;
    screen.press_key(key::RETURN, &[])?;
    for _ in 0..25 {
        let files = admin.files_on("engineering", &text).await?;
        if files.contains(&file) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the sent message does not carry the draft's file {file}"
    )))
}

/// Whether a label sits in the sidebar, which comes after its CHANNELS
/// heading in layout order -- the title bar says the open channel's name
/// too, and comes first.
fn in_sidebar(tree: &[Node], label: &str) -> bool {
    let heading = tree
        .iter()
        .position(|n| n.role == "label" && n.name == "CHANNELS");
    let last = tree
        .iter()
        .rposition(|n| n.role == "label" && n.name == label);
    matches!((heading, last), (Some(h), Some(l)) if l > h)
}

/// A channel archived while the window is open leaves the sidebar at
/// once; the switcher still finds it among the archived; opening it
/// shows what it held with the box greyed; and restoring it brings the
/// row back.
async fn archived_channel(screen: &Screen, admin: &Admin) -> Result<()> {
    const ARCHIVE: &str = "archive-practice";
    const ARCHIVE_LABEL: &str = "Archive Practice";
    screen.open_channel("Engineering").await?;
    admin.restore(ARCHIVE).await.ok();
    screen
        .until("the channel is in the sidebar to begin with", |tree| {
            in_sidebar(tree, ARCHIVE_LABEL)
        })
        .await?;
    admin.archive(ARCHIVE).await?;
    screen
        .until("the archived channel leaves the sidebar", |tree| {
            !in_sidebar(tree, ARCHIVE_LABEL)
        })
        .await?;
    focused_composer(screen).await?;
    screen.press_key(0x6b, &[key::CONTROL])?;
    let finder = screen
        .until("the switcher is up", |tree| {
            tree.has("search box", "Find channel…")
        })
        .await?;
    let entry = finder.find("search box", "Find channel…")?.clone();
    screen.click_into(&entry).await?;
    screen.type_into(&entry, "Archive").await?;
    screen
        .until("the switcher offers it among the archived", |tree| {
            tree.has("label", "ARCHIVED") && tree.has("label", ARCHIVE_LABEL)
        })
        .await?;
    screen.press_key(key::RETURN, &[])?;
    let opened = screen
        .until("the archived channel is open", |tree| {
            tree.has("button", &format!("{ARCHIVE_LABEL}, "))
        })
        .await?;
    // The box is greyed through its pane, which AT-SPI does not pass
    // down to the box's own state; the status line says why, and that
    // is what is checked.
    if !opened.has("label", "this channel is archived") {
        return Err(Error(
            "the window does not say the channel is archived".to_owned(),
        ));
    }
    admin.restore(ARCHIVE).await?;
    screen
        .until("the restored channel is back in the sidebar", |tree| {
            in_sidebar(tree, ARCHIVE_LABEL)
        })
        .await?;
    screen.open_channel("Engineering").await
}

/// The channel menu's editor changes the header on the server, and the
/// socket brings the change back to the menu.
async fn edits_header(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen.press("Engineering").await?;
    screen.press("Edit channel…").await?;
    let dialog = screen
        .until("the channel editor is up", |tree| {
            tree.has("text box", "Header")
        })
        .await?;
    let header = format!("Header set by the lab at {}", crate::seed::stamp());
    screen
        .type_into(dialog.find("text box", "Header")?, &header)
        .await?;
    screen.press("Save").await?;
    let mut last = String::new();
    for _ in 0..25 {
        let record = admin.channel_record("engineering").await?;
        record["header"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut last);
        if last == header {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if last != header {
        return Err(Error(format!(
            "the server holds header {last:?}, not {header:?}"
        )));
    }
    // And the window heard it over the socket: the menu says the new one.
    screen.press("Engineering").await?;
    screen
        .until("the menu shows the new header", |tree| {
            tree.has("label", &header)
        })
        .await?;
    screen.close_menu("Engineering").await
}

/// Archiving from the menu asks first, then the channel leaves the
/// sidebar on the server's word; the lab restores it for next time.
async fn archives_from_menu(screen: &Screen, admin: &Admin) -> Result<()> {
    const ARCHIVE: &str = "archive-practice";
    const ARCHIVE_LABEL: &str = "Archive Practice";
    admin.restore(ARCHIVE).await.ok();
    screen.open_channel(ARCHIVE_LABEL).await?;
    screen.press(ARCHIVE_LABEL).await?;
    screen.press("Archive channel").await?;
    // A moment for the dialog to come up: asked for at once, the tree
    // was once read before it had.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    screen
        .until("the window asks before archiving", |tree| {
            tree.has("label", &format!("Archive {ARCHIVE_LABEL}?"))
        })
        .await?;
    screen.press("Archive").await?;
    screen
        .until("the archived channel leaves the sidebar", |tree| {
            !in_sidebar(tree, ARCHIVE_LABEL)
        })
        .await?;
    let record = admin.channel_record(ARCHIVE).await?;
    if record["delete_at"].as_i64().unwrap_or(0) == 0 {
        return Err(Error(
            "the server does not hold the channel archived".to_owned(),
        ));
    }
    admin.restore(ARCHIVE).await?;
    screen
        .until("the restored channel is back", |tree| {
            in_sidebar(tree, ARCHIVE_LABEL)
        })
        .await?;
    screen.open_channel("Engineering").await
}

/// The overflow menu of a message, brought up the way a person does it.
async fn message_menu(screen: &Screen, text: &str) -> Result<()> {
    let body = screen.text_holding(text).await?;
    screen.scroll_into_view(&body).await?;
    screen.hover(&body).await?;
    screen.click_button_near("More actions", &body).await
}

/// Save and unsave from the message menu, with the server's saved list
/// as the witness.
async fn saves_from_menu(screen: &Screen, admin: &Admin) -> Result<()> {
    const ROOT: &str = "Morning. The nightly run";
    screen.open_channel("Engineering").await?;
    let post = admin.post_id_of("engineering", ROOT).await?;
    for (entry, expected) in [("Save message", true), ("Remove from saved", false)] {
        message_menu(screen, ROOT).await?;
        screen.press(entry).await?;
        let mut saved = false;
        for _ in 0..25 {
            saved = admin.my_saved_ids().await?.contains(&post);
            if saved == expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if saved != expected {
            return Err(Error(format!(
                "after {entry:?} the server holds the post saved = {saved}, not {expected}"
            )));
        }
    }
    Ok(())
}

/// Mark as unread from the message menu puts the channel unread on the
/// server from that message on; coming back to it reads it again.
async fn marks_unread_from_menu(screen: &Screen, admin: &Admin) -> Result<()> {
    const ROOT: &str = "Morning. The nightly run";
    screen.open_channel("Engineering").await?;
    message_menu(screen, ROOT).await?;
    screen.press("Mark as unread").await?;
    let mut unread = 0;
    for _ in 0..25 {
        unread = admin.my_unread("engineering").await?;
        if unread > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if unread == 0 {
        return Err(Error(
            "the server counts nothing unread after Mark as unread".to_owned(),
        ));
    }
    screen.open_channel("Design").await?;
    screen.open_channel("Engineering").await?;
    for _ in 0..25 {
        if admin.my_unread("engineering").await? == 0 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(
        "coming back to the channel did not read it".to_owned(),
    ))
}

/// The profile editor behind the status menu changes the position on
/// the server.
async fn edits_profile(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.press("Set your status").await?;
    screen.press("Edit profile…").await?;
    let dialog = screen
        .until("the profile editor is up", |tree| {
            tree.has("text box", "Position")
        })
        .await?;
    let position = format!("Lab technician since {}", crate::seed::stamp());
    screen
        .type_into(dialog.find("text box", "Position")?, &position)
        .await?;
    screen.press("Save").await?;
    let mut last = String::new();
    for _ in 0..25 {
        admin.my_record().await?["position"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut last);
        if last == position {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(Error(format!(
        "the server holds position {last:?}, not {position:?}"
    )))
}

/// The editor's check makes a public channel private on the server, and
/// the socket tells the window; the lab puts it back for next time.
async fn makes_private(screen: &Screen, admin: &Admin) -> Result<()> {
    const ARCHIVE: &str = "archive-practice";
    const ARCHIVE_LABEL: &str = "Archive Practice";
    admin.set_privacy(ARCHIVE, false).await?;
    admin.restore(ARCHIVE).await.ok();
    // The way back to public is announced to nobody, so a window that
    // signed in while the channel was private still thinks it is: a
    // reload is how it learns, here and for a person.
    screen.press("Reload channels").await?;
    screen.open_channel(ARCHIVE_LABEL).await?;
    screen.press(ARCHIVE_LABEL).await?;
    screen.press("Edit channel…").await?;
    let editor = screen
        .until("the channel editor is up", |tree| {
            tree.has("text box", "Header")
        })
        .await?;
    if !editor.has("checkbox", "Make this channel private") {
        screen.press("Cancel").await?;
        return Err(Error(
            "the editor does not offer to make a public channel private".to_owned(),
        ));
    }
    // A check button offers no AT-SPI action; it is clicked, as a
    // person clicks it.
    let check = editor
        .find("checkbox", "Make this channel private")?
        .clone();
    if let Err(e) = screen.click_into(&check).await {
        screen.press("Cancel").await?;
        return Err(e);
    }
    screen.press("Save").await?;
    let mut kind = String::new();
    for _ in 0..25 {
        admin.channel_record(ARCHIVE).await?["type"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut kind);
        if kind == "P" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if kind != "P" {
        return Err(Error(format!(
            "the server holds the channel as {kind:?}, not private"
        )));
    }
    // The window heard it: the editor no longer offers the check.
    screen.press(ARCHIVE_LABEL).await?;
    screen.press("Edit channel…").await?;
    screen
        .until("the editor is up for a private channel", |tree| {
            tree.has("text box", "Header")
        })
        .await?;
    let offered = screen
        .tree()
        .await?
        .has("check box", "Make this channel private");
    screen.press("Cancel").await?;
    admin.set_privacy(ARCHIVE, true).await.ok();
    admin.set_privacy(ARCHIVE, false).await?;
    screen.press("Reload channels").await?;
    if offered {
        return Err(Error(
            "the editor still offers to make private a channel that is".to_owned(),
        ));
    }
    Ok(())
}

/// The bug the lab found on its first day: the thread's reply count came
/// from whichever of two requests answered second, and was dropped.
async fn reply_count(screen: &Screen) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen
        // Two as seeded, and more on a lab kept across runs, where the
        // scenario after this one adds a reply each time.
        .until("the thread says how many replies it has", |tree| {
            tree.has("button", " replies")
        })
        .await
        .map(|_| ())
}

/// Putting somebody into a channel and taking them out again, checked
/// against the server both times.
async fn add_and_remove(screen: &Screen, admin: &Admin) -> Result<()> {
    let before = admin.members("engineering").await?;
    screen.open_channel("Engineering").await?;
    screen.press("Engineering").await?;
    let menu = screen
        .until("the channel menu is open", |tree| {
            tree.has("label", "ADD PEOPLE")
        })
        .await?;

    screen
        .type_into(menu.find("search box", "")?, "linus")
        .await?;
    screen
        .until("linus is offered", |tree| tree.has("label", "@linus"))
        .await?;
    screen.press("Add").await?;

    screen
        .until("the roster says five", |tree| {
            tree.has("label", "5 MEMBERS")
        })
        .await?;
    let after = admin.members("engineering").await?;
    if after != before + 1 {
        return Err(Error(format!(
            "the window added linus but the server still has {after} member(s), not {}",
            before + 1
        )));
    }

    screen.press("Remove @linus").await?;
    screen
        .until("the roster says four again", |tree| {
            tree.has("label", "4 MEMBERS")
        })
        .await?;
    let back = admin.members("engineering").await?;
    if back != before {
        return Err(Error(format!(
            "the window removed linus but the server has {back} member(s), not {before}"
        )));
    }
    screen.close_menu("Engineering").await?;
    // Tidy up after ourselves: see `sweep_traces`.
    admin.sweep_traces("engineering").await
}

/// The default channel cannot be left, and the button has to say so rather
/// than be pressable and fail.
///
/// Also the one place the `Sensitive` state index is checked against a
/// widget known to be the other way: the AT-SPI state bitfield's layout is
/// not something a compiler can verify.
async fn town_square(screen: &Screen) -> Result<()> {
    if leave_button_works(screen, "Town Square").await? {
        return Err(Error(
            "Town Square's Leave button can be pressed, and the server answers 403 to it"
                .to_owned(),
        ));
    }
    // The other way round on an ordinary channel, because "the button is
    // not pressable" would also be true of a button this test cannot find
    // and of a state bit read from the wrong place.
    if !leave_button_works(screen, "Engineering").await? {
        return Err(Error(
            "no channel's Leave button can be pressed either, so the check above proves nothing"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Open a channel's menu, ask whether Leave can be pressed, and close it.
async fn leave_button_works(screen: &Screen, channel: &str) -> Result<bool> {
    screen.open_channel(channel).await?;
    screen.press(channel).await?;
    let menu = screen
        .until(&format!("{channel}'s menu is open"), |tree| {
            tree.has("button", "Leave channel")
        })
        .await?;
    let works = screen
        .sensitive(menu.find("button", "Leave channel")?)
        .await?;
    screen.close_menu(channel).await?;
    Ok(works)
}

/// Leaving: the question, the answer, the row going away, and the server
/// agreeing.
/// A reply to a followed thread, posted by somebody else while the
/// Threads view is open, shows up in it without a sync.
///
/// The account follows the seeded thread because it was mentioned in
/// one of the replies. Before this, the Threads view was a photograph
/// of the last sync: the reply appeared in the channel at once and in
/// the thread beside it only on the next launch.
async fn new_reply_in_threads(screen: &Screen, admin: &Admin) -> Result<()> {
    screen.open_channel("Threads").await?;
    let before = admin
        .reply_as(
            "grace",
            "engineering",
            "Morning. The nightly run",
            "Found it. One more while you are looking.",
        )
        .await?;
    let expected = format!("{} replies", before + 1);
    screen
        .until("the thread shows the new reply", move |tree| {
            tree.has("label", &expected) && tree.has("button", "Mark this thread read")
        })
        .await?;

    // And opening the thread is reading it: the dot goes, the way it
    // does in every other client, without the mark-read button being
    // pressed. Opened by its reply count, which is a button on the row
    // the way it is under a message.
    let expected = format!("{} replies", before + 1);
    // One fewer, not none: a lab that has run the perf fixture has other
    // followed threads unread, and they are not this test's business.
    let unread = screen
        .until("the thread list is drawn", |tree| {
            tree.has("button", &expected)
        })
        .await?
        .iter()
        .filter(|n| n.role == "button" && n.name == "Mark this thread read")
        .count();
    screen.press(&expected).await?;
    screen
        .until("opening the thread marks it read", move |tree| {
            tree.iter()
                .filter(|n| n.role == "button" && n.name == "Mark this thread read")
                .count()
                < unread
        })
        .await
        .map(|_| ())
}

async fn leaves(screen: &Screen, admin: &Admin) -> Result<()> {
    if !admin.am_in(PRACTICE).await? {
        return Err(Error(format!(
            "not in {PRACTICE} to begin with, so leaving it proves nothing"
        )));
    }
    screen.open_channel(PRACTICE_LABEL).await?;
    screen.press(PRACTICE_LABEL).await?;
    screen.press("Leave channel").await?;
    screen
        .until("the window asks before leaving", |tree| {
            tree.has("label", "Leave Leaving Practice?")
        })
        .await?;
    screen.press("Leave").await?;

    screen
        .until("the row goes out of the sidebar", |tree| {
            !tree.has("label", PRACTICE_LABEL)
        })
        .await?;
    if admin.am_in(PRACTICE).await? {
        return Err(Error(
            "the row went out of the sidebar but the server still has us in the channel".to_owned(),
        ));
    }
    Ok(())
}

/// The client under test, and the promise to stop it again.
#[derive(Debug)]
struct Client(Child);

impl Client {
    fn start(home: &Path) -> Result<Self> {
        Self::spawn(home, &[])
    }

    fn spawn(home: &Path, extra: &[(&str, &str)]) -> Result<Self> {
        // The build tree first, then the installed one. Inside the test
        // container there is no build tree at all -- the client is there
        // because the .deb put it in /usr/bin, which is the whole point of
        // testing against the package.
        let binary = [
            repo_root().join("target/release/mattermost-rs"),
            repo_root().join("target/debug/mattermost-rs"),
            PathBuf::from("/usr/bin/mattermost-rs"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| {
            Error(
                "no client to test: run `cargo build --release --bin mattermost-rs`, or                  install the package"
                    .to_owned(),
            )
        })?;
        for dir in ["state", "cache", "data"] {
            std::fs::create_dir_all(home.join(dir))
                .map_err(|e| Error(format!("cannot create {}: {e}", home.display())))?;
        }
        println!("running {}", binary.display());
        let mut command = Command::new(&binary);
        command
            .env("MMRS_APP_ID", "mmrs.lab")
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_DATA_HOME", home.join("data"));
        for (key, value) in extra {
            command.env(key, value);
        }
        command
            .spawn()
            .map(Self)
            .map_err(|e| Error(format!("could not start {}: {e}", binary.display())))
    }

    /// Asked to go rather than killed outright: a `SIGKILL`ed GTK app leaves
    /// the desktop's crash reporter with something to say about it, and the
    /// client has a stored session to write on the way out.
    /// The same client, told to measure itself.
    ///
    /// On the child's environment rather than this process's: setting a
    /// variable in a running program is `unsafe` in this edition, and the
    /// workspace forbids `unsafe` -- rightly, since the only reason to
    /// want it here is laziness about plumbing.
    fn measuring(home: &Path) -> Result<Self> {
        // Opening the channel has to happen before the sweep starts, and
        // signing in from cold is most of that wait.
        let mode = std::env::var("MMRS_PERF_MODE").unwrap_or_else(|_| "1".to_owned());
        Self::spawn(home, &[("MMRS_PERF", &mode), ("MMRS_PERF_AFTER", "25")])
    }

    /// What the client is holding, before it is asked to stop.
    ///
    /// Keeping a drawn list per channel buys an instant switch with
    /// memory, and a trade is only a trade if both sides are measured.
    fn say_memory(&self) {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.0.id()));
        let Ok(status) = status else {
            println!("memory: no /proc for the client");
            return;
        };
        for line in status.lines() {
            if line.starts_with("VmRSS:") || line.starts_with("VmHWM:") {
                println!(
                    "memory: {}",
                    line.split_whitespace().collect::<Vec<_>>().join(" ")
                );
            }
        }
    }

    fn stop(&mut self) {
        let _ = Command::new("kill").arg(self.0.id().to_string()).status();
        for _ in 0..40 {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Where the client under test keeps its session.
///
/// Under `target/` in a build tree, so `cargo clean` throws it away and
/// nothing lands in the developer's real `~/.local/state`. Inside the test
/// container there is no build tree, and anywhere writable will do --
/// the container is thrown away after the run.
fn lab_home() -> PathBuf {
    let built = repo_root().join("target");
    if built.is_dir() {
        built.join("lab-home")
    } else {
        std::env::temp_dir().join("mmrs-lab-home")
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Re-run `mmlab ui` inside a display and a session bus of its own.
///
/// Three things have to be true before a single widget can be driven, and
/// a CI runner has none of them: an X display for GTK to draw into, a
/// session bus for the accessibility bus to be activated off, and a
/// runtime directory that is not shared with anybody.
///
/// That last one is not housekeeping. `at-spi-bus-launcher` is a
/// singleton keyed by `XDG_RUNTIME_DIR`, and its socket is named after
/// the display -- so a nested session activating it **takes down the one
/// the desktop is already using**. Running this on a workstation emptied
/// the developer's own `at-spi` directory twice before the cause was
/// found, which on a machine somebody is using means the screen reader
/// stops working. A private runtime directory gives this run a bus of its
/// own and leaves the desktop's alone.
///
/// Driven from here rather than from the Makefile because it is logic,
/// and logic in this repository lives in Rust where it is compiled and
/// linted.
pub fn headless(command: &str, passthrough: &[String]) -> Result<()> {
    let runtime = std::env::temp_dir().join(format!("mmrs-ui-{}", std::process::id()));
    make_private(&runtime)?;

    let me = std::env::current_exe().map_err(|e| Error(format!("cannot find my own path: {e}")))?;
    // System directories first: Homebrew ships its own `dbus-run-session`
    // and `dbus-daemon`, and the session bus that daemon starts cannot
    // activate org.a11y.Bus -- GTK then reports the accessibility bus as
    // closed and there is nothing to drive. The rest of PATH is kept after
    // them, because cargo is not in /usr/bin.
    let path = std::env::var("PATH").unwrap_or_default();
    let status = Command::new("xvfb-run")
        .args(["-a", "--server-args=-screen 0 1600x1200x24"])
        .arg("dbus-run-session")
        .arg("--")
        .arg(&me)
        .arg(command)
        .args(passthrough.iter().filter(|a| *a != "--headless"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("PATH", format!("/usr/bin:/bin:{path}"))
        .status()
        .map_err(|e| {
            Error(format!(
                "could not start xvfb-run: {e}
It comes from the `xvfb` package;                  `make apt-deps` installs it along with at-spi2-core and dbus-daemon."
            ))
        })?;
    let _ = std::fs::remove_dir_all(&runtime);
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!("the headless run exited {status}")))
    }
}

/// A directory only this user can read, which is what `XDG_RUNTIME_DIR`
/// has to be or the session bus refuses to put its socket there.
fn make_private(at: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let _ = std::fs::remove_dir_all(at);
    std::fs::create_dir_all(at)
        .map_err(|e| Error(format!("cannot create {}: {e}", at.display())))?;
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| Error(format!("cannot lock down {}: {e}", at.display())))
}

/// Run the whole thing inside a container of its own.
///
/// The strongest isolation available, and the reason to prefer it on a
/// machine somebody is using: the client, its display, its session bus and
/// its accessibility bus are all inside, and none of them is the
/// developer's. Nothing this does can reach the desktop.
///
/// The container shares the **lab server's** network namespace rather than
/// joining its network, so `http://localhost:8065` inside the container is
/// the server -- which means the client needs no different address, and
/// the server's own `SiteURL` still matches what the client connects to.
/// Joining the network by name instead would make the WebSocket's origin
/// disagree with `SiteURL`, and the socket would be refused.
pub async fn contained(
    shots: &Path,
    keep_data: bool,
    perf: Option<&str>,
    only: Option<&str>,
) -> Result<()> {
    // The server first, from out here, because there is no Docker inside.
    if !keep_data {
        crate::stack::down(false)?;
    }
    crate::stack::up().await?;
    // Measuring a scroll needs something to scroll through, and six
    // messages measure nothing; measuring a switch needs channels to
    // wander through, more than the client keeps drawn.
    let switching = std::env::var("MMRS_PERF_MODE").is_ok_and(|mode| mode == "switch");
    crate::seed::seed_with(
        if perf.is_some() && switching {
            TOURED
        } else {
            0
        },
        if perf.is_some() { PERF_MESSAGES } else { 0 },
    )
    .await?;

    build_image()?;
    std::fs::create_dir_all(shots)
        .map_err(|e| Error(format!("cannot create {}: {e}", shots.display())))?;
    let out = shots
        .canonicalize()
        .map_err(|e| Error(format!("cannot resolve {}: {e}", shots.display())))?;

    let mut docker = Command::new("docker");
    docker
        .args([
            "run",
            "--rm",
            "--network",
            "container:mmrs-lab-app",
            "--volume",
        ])
        .arg(format!("{}:/out", out.display()));
    // A container inherits nothing from this shell, and what is being
    // measured is chosen out here: the mode, and whatever else an
    // experiment is turning on and off.
    for (name, value) in std::env::vars() {
        if name.starts_with("MMRS_") {
            docker.arg("--env").arg(format!("{name}={value}"));
        }
    }
    docker.arg(IMAGE);
    if let Some(channel) = perf {
        println!("measuring {channel} inside {IMAGE}");
        docker.args(["perf", "--headless", "--guest", "--channel", channel]);
    } else {
        println!("running the scenarios inside {IMAGE}");
        // The image's own command, restated so one scenario can be asked
        // for: a run is minutes, and a scenario being worked on is seconds.
        docker.args(["ui", "--headless", "--guest", "--shots", "/out"]);
        if let Some(only) = only {
            docker.args(["--only", only]);
        }
    }
    let status = docker
        .status()
        .map_err(|e| Error(format!("could not run docker: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!(
            "the contained run exited {status}. Screenshots, if any, are in {}",
            out.display()
        )))
    }
}

const IMAGE: &str = "mmrs-ui:latest";

/// How much history the scroll measurement needs. Enough to page through
/// several times, which is what makes the splice show up at all.
const PERF_MESSAGES: usize = 2000;

/// How many times to go back and forth between two channels. Enough
/// that the second visit to each is in the trace, which is the visit
/// that should cost nothing.
const SWITCHES: usize = 4;

/// How many further channels the switch run wanders through. More than
/// the client keeps drawn, so the oldest are dropped on the way.
const TOURED: usize = 16;

/// Build the image the scenarios run in.
///
/// Every time, not only when it is missing: the whole point is to test
/// the binary as it is now, and `docker build` is a few seconds when
/// nothing but the last two layers changed.
fn build_image() -> Result<()> {
    let root = repo_root();
    for needed in ["dist", "target/release/mmlab"] {
        if !root.join(needed).exists() {
            return Err(Error(format!(
                "{needed} is missing. `make deb` builds the package the image installs, and                  `cargo build --release -p mm-lab` builds the driver it runs."
            )));
        }
    }
    println!("building {IMAGE}");
    let status = Command::new("docker")
        .args([
            "build",
            "--file",
            "docker/ui.Dockerfile",
            "--tag",
            IMAGE,
            ".",
        ])
        .current_dir(&root)
        .status()
        .map_err(|e| Error(format!("could not run docker build: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!("docker build failed with {status}")))
    }
}

/// Measure scrolling, which is the one thing about this window that
/// cannot be argued about from the source.
///
/// Signs in, opens the channel, and leaves the client to sweep it: the
/// client scrolls itself a fixed number of pixels per frame and prints
/// the distribution of frame times when it reaches the bottom. Driving it
/// from in here rather than from a hand on a wheel is what makes two
/// measurements comparable -- the same pixels, at the same rate, past the
/// same messages.
pub async fn measure(channel: &str, guest: bool) -> Result<()> {
    if guest {
        crate::stack::wait_ready().await?;
    } else {
        crate::stack::up().await?;
    }

    let home = lab_home();
    let _ = std::fs::remove_dir_all(&home);
    let mut client = Client::measuring(&home)?;
    let screen = Screen::attach_waiting("mmrs.lab").await?;
    signs_in(&screen).await?;
    screen.open_channel(channel).await?;

    if std::env::var("MMRS_PERF_MODE").is_ok_and(|mode| mode == "switch") {
        // What the complaint is actually about: not scrolling one
        // channel but going back and forth between two, where a client
        // that holds its own state should never wait twice for the same
        // one. Back and forth, slowly enough that each has settled
        // before the next click.
        println!("switching between {channel} and Engineering; the client traces each click");
        for round in 0..SWITCHES {
            for name in [&"Engineering", &channel] {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                screen.open_channel(name).await?;
            }
            // Somebody's status moves while the channel is open, the
            // way it does all day on a real server. The trace says
            // what that cost the rows on screen.
            crate::seed::set_status("ada", if round % 2 == 0 { "away" } else { "online" }).await?;
        }

        // And then a tour, because the client keeps a whole drawn list
        // per channel and the question that answers is what a reader
        // who wanders costs. Far enough to pass the limit and start
        // dropping the oldest.
        println!("touring {TOURED} channels, then back");
        for n in 0..TOURED {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            screen.open_channel(&format!("Bulk {n:03}")).await?;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        screen.open_channel(channel).await?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        client.say_memory();
        client.stop();
        return Ok(());
    }

    println!("opened {channel}; the sweep starts shortly and prints its own report");

    // Long enough for the delay the client waits before sweeping, the
    // sweep itself, and the pauses at the top while older messages are
    // fetched. It prints as it finishes; this is only how long to leave
    // it alive.
    tokio::time::sleep(std::time::Duration::from_secs(150)).await;
    client.stop();
    Ok(())
}
