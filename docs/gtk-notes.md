# GTK findings

Measured on this machine, not inferred: Ubuntu 24.04, X11, GNOME Shell 46.0,
GTK **4.14.5** and libadwaita **1.5.0** from apt (Homebrew's 4.22.4 excluded
via `PKG_CONFIG_LIBDIR` — see `cargo xtask doctor`).

These were the two risks the plan called most likely to blow the schedule.
Both are retired. The spike sources are throwaway and live outside the repo.

## The tray works, with a caveat worth stating

The plan assumed this might be undeliverable, because GNOME dropped
StatusNotifierItem support. On **Ubuntu** GNOME it is fine:

- `ubuntu-appindicators@ubuntu.com` is installed **and enabled by default**.
- `org.kde.StatusNotifierWatcher` is present on the session bus and
  `IsStatusNotifierHostRegistered` is `true`.
- A real `ksni` 0.3.7 item registered successfully and appeared in the
  watcher's `RegisteredStatusNotifierItems`, alongside other applications
  already using it.

So **tray stays in v1 parity.** The caveat: it depends on that extension. On
vanilla GNOME, or if the user disables it, the watcher is simply absent.
`mm-desktop-xdg` must therefore **check for the watcher and degrade visibly**
— if there is no host, say so once rather than publishing an item into the
void and leaving the user wondering where the icon went.

Note for the dependency invariant: `ksni` pulls in **tokio and zbus 5.19**, so
it belongs in `mm-desktop-xdg` (ungated). `cargo xtask check-deps` rejects it
in `mm-desktop`, which is correct — the tray *model* is pure, the registration
is not. (`ksni` does offer a `blocking` feature if avoiding tokio ever
matters; `mm-http` brings tokio anyway, so it does not today.)

## `gtk::ListView` handles a chat scrollback correctly

Tested with **variable-height rows** (1–4 wrapped lines, cycling), 400 rows,
a 520×420 window. Uniform rows let ListView take a fast path that hides the
problem, so varying them is the whole point.

| Question | Result |
|---|---|
| Prepend 50 rows **above** the viewport | **Viewport preserved.** Δvalue 3764px vs Δupper 3728px — they track within less than one row's height. |
| Append while sitting at the bottom | The newest row stays *at least partially* visible, but see below. |
| Append while **scrolled up** reading history | **Zero drift.** value 9312 → 9312 while upper grew 17800 → 18000. |

**What this means for `PostList`:**

1. **Backfilling older messages needs no scroll-anchor fixup.** This was the
   plan's single riskiest GTK unknown — the fear was having to capture the top
   visible row's id and y-offset before a splice and restore it in an idle
   callback. Not needed: ListView already does the right thing.
2. **There is no yank bug to defend against.** ListView never auto-scrolls on
   append, so a user reading history is left alone for free.
3. **Stick-to-bottom must therefore be implemented explicitly.** Because (2)
   holds, "follow new messages when I am at the bottom" does not come for
   free. The "newest row still visible" result above is weaker than it looks —
   the row was only *overlapping* the viewport by a few pixels, in the 36px of
   slack that happened to be there. Treat it as "do not rely on it".

### Scrolling to the bottom: what actually works

The spike reported that `scroll_to(last)` "re-pins reliably". That was true of
what the spike did and **misleading as general advice**, because the spike
called it from a timer with the list already laid out. Building the real
window turned up what that hid. Three approaches, in the order they were
tried:

| Approach | Result |
|---|---|
| `scroll_to` immediately after filling the model | **Lands short.** ListView has not measured the rows it was just handed. |
| `scroll_to` from `glib::idle_add_local_once` | **Also lands short.** An idle runs before the layout pass, not after it. |
| Pinning `vadjustment` from its own `changed` handler | **Does not take.** Setting an adjustment's value from inside its own `changed` signal is re-entrant and GTK ignores it. |
| `scroll_to` from `glib::timeout_add_local_once(~120ms)` | **Works.** A timer yields to the layout pass first. |

So: scrolling to the end needs a layout pass to have happened, and the only
one of these that guarantees it is a timer. That is unsatisfying — it is a
delay chosen to be "long enough" — but it is what was measured, and the
alternatives were measured not to work.

The adjustment handlers are still worth having for messages arriving *later*,
when the list is already measured. **That path is not yet verified**: it needs
a message to arrive while the window sits settled at the bottom, which was not
reproducible by screenshot while the desktop was in use.

### The trap: `vadjustment.upper()` is an estimate, not a measurement

This is the finding worth the most. With variable-height rows, ListView
*estimates* the total height and revises it as rows are realized. Observed
within single three-second runs:

```
17818 -> 21346                    (converged after scroll_to, then stable)
37350 -> 46674 -> 37474           (a 20% revision, in both directions)
```

So **never compute "am I at the bottom?" as `value ≈ upper - page_size`.** It
is arithmetic against a moving number, and it will be flaky in exactly the way
that gets a test muted rather than fixed. My own first attempt at the
stick-to-bottom spike did this and produced a confident, wrong answer.

Ask a stable question instead: is the last row realized and inside the
viewport (walk `ListView`'s realized children and use `compute_bounds`), or
simply track the user's own scroll intent and use `scroll_to`.

A second-order consequence: `upper` only converges once rows near the
viewport have been realized, so any measurement taken immediately after a
splice or a `scroll_to` is premature. Wait a tick.

## Still untested

- Prepending while at the **very top** (offset 0), where there is no room
  above to absorb the insertion.
- 100k rows. These spikes used 400; the estimate revisions above will be
  proportionally larger, which matters for the `upper` trap but not for the
  viewport-preservation result.
- `TextView`-based rows. All three spikes used `gtk::Label`, which is the fast
  path `mm-md::is_plain()` will emit. Rows containing a `TextView` with inline
  custom emoji are heavier and need their own measurement before P2.
- Wayland. X11 only so far.

## Capturing one window when two instances are running

`MMRS_APP_ID` overrides the GTK application id *and* appends it to the window
title, so a test instance is `mattermost-rs [mmrs.lab]` while the one you use
all day is plain `mattermost-rs`. That exists because `xwininfo -root -tree`
prints the WM **class** on every line:

```
0x2e00004 "mattermost-rs [mmrs.lab]": ("mattermost-rs" "mattermost-rs") ...
0x2c00004 "mattermost-rs":            ("mattermost-rs" "mattermost-rs") ...
```

so matching `"mattermost-rs"` matches the class and therefore *both* windows.
Twice during development that photographed the real session instead of the
test one. Match the bracketed title and nothing else:

```
xwininfo -root -tree | grep -F "\"mattermost-rs [$MMRS_APP_ID]\"" | awk '{print $1}'
```

`xdotool` and `wmctrl` are not installed on this machine; `import -window <id>`
from ImageMagick is.

## Bottom alignment: tried twice, dropped

A channel with four messages in it looks better resting them on the
composer than stranding them at the top of an empty pane, which is what the
Electron client does. `valign: End` on the `ListView` achieves that, and it
was shipped with a caveat: the alignment makes `ListView` ask for its
**natural** height rather than taking the viewport's, and the viewport's
height is what virtualization depends on.

The caveat turned out to understate it. For a virtualised list the natural
height is an *estimate*, and when the estimate exceeds the real content the
surplus has to go somewhere — bottom-aligned, it goes above the first
message. In a real channel that was nearly three hundred pixels of blank at
the top, which survived scrolling and looked like a rendering failure.

So: **no bottom alignment.** A short channel's messages sit at the top with
space below them, which is a cosmetic difference from the Electron client.
A blank gap above the first message is not cosmetic. If this is ever worth
revisiting it needs a measured content height — not `valign` — and the
measurement has to come from realized rows, which is the same trap as
`vadjustment.upper()` above.

## Two things about avatars that cost a screenshot each

**A hidden widget still claims its height.** Grouped messages keep the
avatar gutter so their text stays in line with the message that named the
author. Hiding the avatar with `set_opacity(0.0)` leaves a 32-pixel-tall
widget in the row, which pushed every line of a block a row apart and undid
the grouping it was meant to preserve. `set_visible(false)` plus a left
margin on the text column is what actually works.

**Icon names are not checked at build time.** `gtk::Image::from_icon_name`
with a name the theme does not have renders a broken-image placeholder and
logs nothing at the default level. Mattermost's own `channel-public-symbolic`
and `channel-private-symbolic` are not Adwaita names. Verified against
`/usr/share/icons/Adwaita` before using one; a public channel is a `#` label
because Adwaita has no hash icon worth approximating.

`adw::Avatar` is the right widget for a face: it draws initials from a name
when it has no image, so a row looks finished before the network answers,
and it crops whatever it is given to a circle.

## Drawing a channel: where 2.3 seconds went

The sidebar cache made the window paint at once and exposed what had been
hidden behind the wait: a channel of sixty messages took **2.3 seconds** to
draw, and the launch drew it six times, so the messages arrived ten to
fifteen seconds in. Measured end to end, with timing marks in the engine
and around the GTK work.

**The engine was not the problem.** Every request a launch makes was
finished in 995 ms, of which the channel's own page was done at 894 ms. All
of the rest was on the GTK thread.

Three findings, each measured rather than reasoned about:

**One splice, not ninety-five appends.** Filling a `gio::ListStore` with
`append` emits `items_changed` per item, and `ListView` answers each one by
measuring what it holds. Building the rows into a `Vec` and handing them
over with `splice(0, n_items, &rows)` is one signal. (This did not move the
number on its own — see below — but it is still the right way to fill the
model, and it is what makes the remaining cost legible.)

**`ListView` realizes every row it is given, not the visible ones.** With a
601-pixel viewport and ninety-five rows it ran `setup` and `bind` ninety-five
times, every time. Whatever the reason — and `measure_across` skipping
unrealized tiles says it is not the obvious one — the practical consequence
is that **the cost of a row is paid for the whole channel, not for the
screenful on show.** Anything expensive in a row has to be lazy.

**The hover toolbar was ninety-four per cent of it.** Handing `ListView`
102 rows took **2411 ms** with the toolbar built in `setup` and **145 ms**
with it left out. It is not any one widget in it: taking only the
`GtkPopover` out changed nothing, and swapping the three icon buttons for
plain labels saved about half. Six widgets, a controller and two closures,
around 22 ms a row on this machine.

So the toolbar is built the first time the pointer enters the row, from a
`GtkEventControllerMotion` added in `setup`, and never at all for a row
nobody hovers over. The overflow menu goes one step further: it is made
*and* filled in `GtkMenuButton::set_create_popup_func`, which GTK calls
just before popping it up. Filling it had been done on every bind, because
its entries depend on the message — pin or unpin, save or unsave, delete
only your own.

Drawing a channel is now 146 ms.

The matching lesson on the other side of the bridge: **coalesce redraws.**
A sync answers with teams, channels, members, categories and the roster of
each team in turn, and each answer redrew the sidebar and the open channel.
`redraw_soon` (100 ms) and `sidebar_soon` (50 ms) collapse a burst into one
redraw; the WebSocket arm uses the former too, since a reconnect replays a
queue of events.

## Redrawing a channel: hand the list only what changed

The measurements above said `ListView` builds a widget set for every row
it is handed, and that turned out to be literal: a hundred-row channel
reports a hundred `setup` calls, every time, with a 601-pixel viewport.
Virtualisation does not happen here, and the scroll policy is not the
reason — the view's `vscroll-policy` is already `Minimum`.

Taken with `refresh_posts` rebuilding the whole model, that made every
redraw cost the whole channel: an avatar landing, an image decoding, a
message arriving, a page of history loaded. And every page of history
made the next redraw worse, so a reader who scrolled up a few times
arrived at a window that stopped answering — which is exactly what it
looked like from the outside.

So the model is handed only the run that differs. Building the rows is
free (under a millisecond for a channel); comparing them is `PostRow::matches`,
which compares everything a row draws, textures included. Two cases,
because a message list only ever does two things:

  - **The length changed** — history prepended, a message appended. A
    common prefix and a common suffix find the inserted run exactly.
  - **The length is the same and some rows are redrawn** — every avatar,
    image, reaction and edit. Those are replaced one at a time, because a
    prefix-and-suffix diff is worst at precisely this: one person's avatar
    arriving changes every row they wrote, scattered down the channel, and
    the run from the first to the last of them is most of it.

Measured on the same channel, 102 rows:

| | realized | time |
|---|---|---|
| before any of this | 95 of 95 | 2411 ms |
| lazy hover toolbar, whole model respliced | 102 of 102 | 146 ms |
| …and only the prefix/suffix respliced | 92 | 130 ms |
| …and same-length rows replaced one by one | 16 | 42 ms |
| a redraw where nothing changed | 0 | 0 ms |

What is left: the first draw of a channel still realizes every row in it,
because every row is new. That is a one-off per channel opened, and at
about 1.4 ms a row it is 150 ms for a hundred. If it ever matters, the
thing to find out is why `ListView` does not virtualise here — the obvious
candidates (scroll policy, `hscrollbar-policy: never`, bottom alignment)
have all been ruled out by measurement.
