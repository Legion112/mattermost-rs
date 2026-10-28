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

use crate::seed::Admin;
use crate::ui::{Find, Screen};
use crate::{Error, Result};

/// Every scenario, in the order they run.
///
/// Order is part of the test: signing in has to come first, and
/// `leaves-a-channel` is last because it is the only one that changes what
/// the sidebar holds.
pub const SCENARIOS: &[&str] = &[
    "signs-in",
    "shows-a-thread-reply-count",
    "adds-and-removes-somebody",
    "refuses-to-leave-town-square",
    "leaves-a-channel",
];

/// The channel the seed makes for exactly this, so that joining and leaving
/// costs nothing.
const PRACTICE: &str = "leaving-practice";
const PRACTICE_LABEL: &str = "Leaving Practice";

pub async fn run(only: Option<&str>, shots: &Path, keep_data: bool) -> Result<()> {
    // A fresh server unless told otherwise, and this is not fussiness.
    // Every run of `adds-and-removes-somebody` leaves two system notices in
    // Engineering -- "linus added", "linus removed" -- and after enough runs
    // they push the seeded thread off the channel's first page, at which
    // point `shows-a-thread-reply-count` starts failing for a reason that
    // has nothing to do with reply counts. A test whose result depends on
    // how many times it has been run before is not a test.
    if !keep_data {
        crate::stack::down(false)?;
    }
    crate::stack::up().await?;
    crate::seed::seed().await?;
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
        if only.is_some_and(|wanted| wanted != *name) && *name != "signs-in" {
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
        "shows-a-thread-reply-count" => reply_count(screen).await,
        "adds-and-removes-somebody" => add_and_remove(screen, admin).await,
        "refuses-to-leave-town-square" => town_square(screen).await,
        "leaves-a-channel" => leaves(screen, admin).await,
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

/// The bug the lab found on its first day: the thread's reply count came
/// from whichever of two requests answered second, and was dropped.
async fn reply_count(screen: &Screen) -> Result<()> {
    screen.open_channel("Engineering").await?;
    screen
        .until("the thread says how many replies it has", |tree| {
            tree.has("button", "2 replies")
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
    screen.close_menu("Engineering").await
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
        let binary = ["target/release", "target/debug"]
            .into_iter()
            .map(|dir| repo_root().join(dir).join("mattermost-rs"))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                Error(
                    "no client to test: run `cargo build --release --bin mattermost-rs` first"
                        .to_owned(),
                )
            })?;
        for dir in ["state", "cache", "data"] {
            std::fs::create_dir_all(home.join(dir))
                .map_err(|e| Error(format!("cannot create {}: {e}", home.display())))?;
        }
        println!("running {}", binary.display());
        Command::new(&binary)
            .env("MMRS_APP_ID", "mmrs.lab")
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_DATA_HOME", home.join("data"))
            .spawn()
            .map(Self)
            .map_err(|e| Error(format!("could not start {}: {e}", binary.display())))
    }

    /// Asked to go rather than killed outright: a `SIGKILL`ed GTK app leaves
    /// the desktop's crash reporter with something to say about it, and the
    /// client has a stored session to write on the way out.
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

fn lab_home() -> PathBuf {
    repo_root().join("target/lab-home")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."))
        .to_path_buf()
}
