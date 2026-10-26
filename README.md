# mattermost-rs

A native Linux Mattermost client in Rust. One binary, no Chromium, no Node.

This is a from-scratch reimplementation, not a fork. The official desktop app is
an Electron shell around the server's web app; this talks the Mattermost REST
v4 API and WebSocket event stream directly and draws its own GTK4 interface.

**Status: early. Not yet usable.** See [docs/parity.md](docs/parity.md) for what
works, and the Phases section below for what lands when.

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

Against your own server, with a personal access token (Account Settings →
Security → Personal Access Tokens):

```bash
make release
./target/release/mattermost-rs token --server https://your.mattermost --token <pat>
./target/release/mattermost-rs sync      # channels and unread counts, in the terminal
./target/release/mattermost-rs gui       # the window
```

`login --server <url> --login-id <id> --password-env <VAR>` works too; the
password is read from the environment rather than an argument so it stays out
of shell history and out of `ps`. SSO accounts need a token: the browser-based
desktop-token hand-off is not implemented yet.

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

Beyond that there is a Docker lab (`make lab`, from P1) that runs a real
Mattermost server and drives `mm-headless` against it, and a golden suite
replaying traces recorded from a live server.

### What the tests cannot prove

Worth stating, because the failure mode is believing a green check means more
than it does:

- **Any pixel.** The row projection and all boundary logic are pure and gated,
  but nothing in CI renders a frame. A GUI that draws unread badges in the
  wrong place passes everything.
- **That a notification is delivered.** The tests assert the `Notifier` trait
  was called with the right content, not that GNOME accepted it.
- **The tray.** GNOME dropped StatusNotifierItem support; whether a tray icon
  appears at all depends on a third-party extension being installed.
- **HiDPI, fractional scaling, IME, clipboard, window-manager focus.**
- **Scale.** The lab fixture is a handful of users and ~170 posts. It says
  nothing about 50 teams or a 10k-post channel.

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
