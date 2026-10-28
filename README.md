# mattermost-rs

A native Linux Mattermost client in Rust. One binary, no Chromium, no Node.

This is a from-scratch reimplementation, not a fork. The official desktop app is
an Electron shell around the server's web app; this talks the Mattermost REST
v4 API and WebSocket event stream directly and draws its own GTK4 interface.

**Status: early, but it runs.** It logs in, lists channels with correct unread
and mention counts, shows a channel's messages, sends messages, follows live
WebSocket events, and raises desktop notifications. There is no markdown
rendering, no threads, no files, no search, and no reconnect backfill yet.

See [docs/parity.toml](docs/parity.toml) for feature-by-feature status,
including which behaviours have been verified against a live server and which
have not.

## Purpose

To own my chat client. The Electron app ships ~250 MB and a Chromium per
window to render text; a native client should cost a fraction of that, start
instantly, and — because the interesting logic is pure — be provable rather
than hopefully-correct.

## Architecture

The workspace is split along one axis that matters more than any other: which
crates are inside the **100% line-coverage gate**. A gated crate is pure logic
with no sockets, no GTK and no filesystem except through a trait.

| Crate | Gate | Role |
|---|---|---|
| `mm-types` | — | wire and domain types; covered by golden fixtures instead |
| `mm-api` | **100%** | REST v4 + WebSocket protocol, synchronous and pure |
| `mm-core` | **100%** | the state machine: store, reducers, unread, notify policy |
| `mm-md` | **100%** | markdown + Mattermost extensions → abstract document |
| `mm-config` | **100%** | config schema, migrations, Electron config import |
| `mm-desktop` | **100%** | deep-link parsing, tray menu model, `.desktop` rendering |
| `mm-desktop-xdg` | — | the D-Bus / Secret Service implementations |
| `mm-http` | — | reqwest + tokio-tungstenite adapters, the Tokio runtime |
| `mm-gui` | — | the GTK4 / relm4 shell, thin by rule |
| `mm-headless` | — | runs `mm-core` against a live server; what the lab drives |
| `mm-lab` | — | the Docker integration lab |

`cargo xtask check-deps` enforces the invariant the gate rests on: no gated
crate may transitively reach `tokio`, `reqwest`, `gtk4`, `glib`, `zbus`,
`rusqlite` or `bollard`. It also fails when a new workspace member is in
neither list, so a crate cannot silently join or dodge the gate.

### Why `mm-api` has no `async fn`

Request builders return a plain `HttpRequest`; response parsers take an
`HttpResponse`; the WebSocket is a state machine fed frames as strings. The
sockets live in `mm-http`. That split is the whole reason every route, every
query-parameter edge case and every error path is a table test rather than an
integration test.

### Why the reducer has no clock

`reduce(&mut State, Envelope) -> Outcome` takes the receive timestamp on its
envelope, and timers are data (`Effect::SetTimer` → `Input::Timer`). So it is a
total function of its inputs: a recorded trace replays byte-for-byte, and the
crate under the strictest coverage requirement needs no clock fake at all.

## Out of scope, permanently

Calls, server plugins and all plugin-provided UI, Boards, Playbooks, and the
interactive Apps framework. These ship **as web app code served by the
server** — no amount of Rust renders them without a webview. Each gets an
"open in browser" escape hatch and a `blocked-no-webview` row in the parity
matrix with its reason and fallback stated.

Also out for now: Windows and macOS, spellchecking, i18n beyond English.

## Try it

```bash
make release
./target/release/mattermost-rs
```

With no stored session the window opens on a sign-in screen: server address,
email or username, password. A two-factor field appears only if the server
asks for one. The password is sent to your server and nothing else — it is
never written to disk, never logged, and never passed as an argument; only the
session token the server returns is stored.

There are terminal equivalents, which is how the stack gets tested without a
window:

```bash
./target/release/mattermost-rs sync      # channels and unread counts
./target/release/mattermost-rs send --channel town-square --text "hello"
./target/release/mattermost-rs watch     # live WebSocket events
./target/release/mattermost-rs doctor    # is anything listening for notifications?
./target/release/mattermost-rs token --server <url> --token <pat>
```

`login --server <url> --login-id <id> --password-env <VAR>` reads the password
from the environment rather than an argument, so it stays out of shell history
and out of `ps`. It goes through the same code as the window.

SSO-only accounts need a personal access token (Account Settings → Security):
the browser-based desktop-token hand-off is not implemented yet.

### Installing it

```bash
cargo xtask install-desktop   # icon and .desktop entry in ~/.local/share
make deb                      # dist/mattermost-rs_<version>_<arch>.deb
```

The first needs no root and uninstalls by deleting two files (`cargo xtask
uninstall-desktop`); add `--autostart` to also start it with the session, in
the tray. The second builds a system-wide package with `dpkg-deb`: the binary
in `/usr/bin`, the same icon and entry under `/usr/share`, and a control
stanza that depends on GTK and libadwaita while only *recommending* the
dictionaries and the GNOME AppIndicator extension — without those the client
still runs, it just has no spelling underlines and no tray.

Either way the desktop learns the `x-scheme-handler/mattermost` association,
so a `mattermost://` link opens this client, and the icon is deliberately not
Mattermost's own mark: this is a third-party client and that logo is somebody
else's trademark.

### A throwaway server to try it against

A disposable Mattermost runs in one container, which is what the development
of this was verified against:

```bash
docker run -d --name mmrs-lab -p 8065:8065 mattermost/mattermost-preview
```

The first account you create on an empty server is automatically a system
admin. Remove it with `docker rm -f mmrs-lab`.

## Build

```
make apt-deps     # system build dependencies (needs sudo)
make doctor       # check the environment, especially which GTK pkg-config finds
make check        # fmt, clippy, tests, dependency invariant, the 100% gate
make ci           # exactly what CI runs — `check` plus `cargo audit`
make release
```

`make doctor` exists for one specific trap: Homebrew installs 403 `.pc` files
that precede the system ones and describe a GTK (4.22) which is **not** the GTK
this machine runs (apt's 4.14). Building against them yields a binary linked to
`/home/linuxbrew/.linuxbrew/lib/libgtk-4.so` that matches neither CI nor the
`.deb`, and the symptom is a confusing runtime crash rather than a build error.
The Makefile and `.cargo/config.toml` both pin `PKG_CONFIG_LIBDIR` to exclude
it; `doctor` asserts the result.

## Testing

Unit and property tests run with no Docker and no network: `make test`. The
coverage gate is `make cover-gate`, and it is gated on **lines**, not regions —
`assert_eq!(a, b, "{x}")` emits a region for the failure-message formatting
that no passing test can reach, so 100% regions would mean assertions with no
failure messages.

Beyond that there is a **Docker lab**: `make lab-up` brings a Postgres and a
Mattermost up on loopback in about sixteen seconds, and `make lab-seed` fills
it with a team, five colleagues, a thread, an attachment and a custom emoji.
`make lab-run` points the client at it with a stored session of its own, so
the one you use all day stays signed in to your real server. It exists because
several things — leaving a channel, adding somebody, removing them — post a
system message into a channel other people are reading, and so cannot honestly
be tried anywhere else.

`make ui-test-docker` drives the client's **real widgets** against that lab,
with the client, its display, its session bus and its accessibility bus all
inside a container of their own. Use this one on a machine you are working
at: `at-spi-bus-launcher` is a singleton keyed by `XDG_RUNTIME_DIR`, and a
test run that shares the desktop's takes it down — which, if you use a
screen reader, means your screen reader stops. The container shares the lab
server's network namespace, so `localhost:8065` inside it is the server and
the client needs no special address; and it installs the `.deb` this
repository builds, so the package's dependency list is under test too — a
missing `Depends` shows up as a window that will not start, here rather than
on somebody's machine.

`make ui-test` does the same without the container, in an Xvfb display and a
session bus of its own. Faster, and what CI runs, because a runner has no
desktop to protect.

Either way, it is the real widgets. GTK publishes its whole widget tree on
the accessibility bus, the same one a screen reader uses, so a test says
"press the button called Leave channel" and means the button. What that bus
cannot do -- Return in a box, a chord, a click into a paragraph, resting the
pointer on a message, turning the wheel -- the driver puts on the display
itself over XTEST, the way a keyboard and a mouse would; a widget's place
comes from its own toplevel's X window, because the bus answers (0, 0) for
every screen position. Each scenario then asks the *server* whether it
actually happened, because a window agreeing with itself is not the same as
a window being right. `--only a,b` runs the named ones, in the container
too, and the last scenario sweeps every trace the others left so a lab kept
across runs stays the shape the seed made. It runs headless — `xvfb-run` for a
display, `dbus-run-session` for the bus the accessibility bus activates off —
and a failure writes a screenshot of the window into `target/ui-failures/`,
which CI keeps as an artefact.

Writing those tests keeps finding things nothing else had: a thread's reply
count that depended on which of two requests answered first, a channel menu
whose button announced its entire contents to a screen reader, a crash when
the channel name was activated twice from the keyboard; and in one night of
writing thirty more, a typing frame read from the wrong half of the message,
a mention arriving live that never raised the badge, a first direct message
that reached nothing until the next sync, a mention of yourself drawn as
plain text in a channel you had not spoken in, a "New messages" line above
your own message, and a webapp draft whose files were dropped on send.

### What the tests cannot prove

Worth stating, because the failure mode is believing a green check means more
than it does:

- **Any pixel.** The UI tests read the widget tree, not the screen: a widget
  can be correct there and invisible, behind another one, or three pixels
  wide. Nothing in CI looks at a frame, so a window that draws unread badges
  in the wrong place passes everything.
- **Anything a pointer does.** The UI tests activate widgets through the
  accessibility bus, which bypasses pointer grabs — so the bug where "Leave"
  needed two clicks, because an open popover ate the first one, would pass
  them. That class of thing still wants a human and a mouse.
- **That a notification is delivered.** The tests assert the `Notifier` trait
  was called with the right content, not that GNOME accepted it.
- **The tray.** GNOME dropped StatusNotifierItem support; whether a tray icon
  appears at all depends on a third-party extension being installed.
- **HiDPI, fractional scaling, IME, clipboard, window-manager focus.**
- **Scale.** The lab fixture is a handful of users and ~170 posts. It says
  nothing about 50 teams or a 10k-post channel.

## What the window does today

Verified by driving the real window against a Mattermost 12.0.1 server in
Docker, not by reading the code:

- **Sign in** from the window with a server, email or username, password and
  MFA code. The password never reaches the environment, an argument or disk.
- **The sidebar is the server's**: your categories, in your order, with your
  sorting — Favorites, Channels, Direct Messages and any of your own. Public
  channels carry a `#`, private ones a padlock, DMs the other person's face.
  Two rows that would read the same are told apart by username or team.
- **Messages** with markdown, links, emoji shortcodes, attachments named and
  sized, date separators, author grouping, avatars, and a "New messages"
  line where you left off.
- **Reactions**, shown in the order each emoji first appeared, and a chip
  you click to add or take back your own.
- **Threads** in a panel on the right, with replies arriving live. With
  collapsed reply threads — the server default since 12.1 — this is the only
  place replies appear at all.
- **Scrollback**: reaching the top loads the page before it and keeps your
  place.
- **Live**: new messages, edits, reactions and read state arrive over the
  WebSocket and the view follows if you are at the bottom.

Not yet: search, file download and preview, editing or deleting your own
messages, mention autocomplete, typing indicators, presence, preferences.

## Phases

| | |
|---|---|
| **P0** | skeleton, the coverage gate, types, transport traits, config + Electron import, login |
| **P1** | read-only chat: WebSocket, the store, the sidebar, the post list |
| **P2** | send and participate — **daily-drivable ends here** |
| **P3** | notifications, badge, tray, deep links, single instance |
| **P4** | search and files |
| **P5** | preferences and polish |
| **P6** | optional SQLite cold-start cache |

## Licence

MIT.

## Where the unit tests live

Gated crates keep their unit tests in `src/tests/<module>.rs`, wired up as:

```rust
#[cfg(test)]
#[path = "tests/ws.rs"]
mod tests;
```

They are still child modules, so `use super::*` reaches private items — but
they sit under a path the coverage gate's ignore regex matches. That matters:
coverage should answer "is the production code exercised?", and a test's own
unexecuted `panic!("unexpected: …")` arm is not a gap in production code.
Before this split, 29 of 30 reported-uncovered lines were assertion messages
and unreachable match arms inside test modules.

`cargo xtask coverage` computes the verdict from cargo-llvm-cov's JSON export
rather than its `--fail-under-*` flags, which have two accounting bugs in
0.9.1 — see the comment on `cover-gate` in the Makefile.
