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

## Bottom alignment, and the measurement it still owes

`ListView` has `valign: End`, so a channel with four messages rests them on
the composer instead of stranding them at the top of an empty pane. That is
what the Electron client does and the difference is immediately visible.

It carries a risk worth writing down. `ListView` only realizes the rows in
view, and that depends on `ScrolledWindow` handing it the *viewport* height.
`valign: End` asks for its natural height instead. Measured with a 1200-post
channel in the lab the window still opened instantly and landed on the newest
message — but the store only ever held the newest page of 60, because
scrollback paging is not implemented yet, so the model never grew past one
screen's worth either way. **That measurement proved the common path and not
the virtualization.**

Backfill has since landed, so a channel's model now grows without limit as
the reader scrolls up. Rather than leave the question open, the alignment is
applied **only while the model holds fewer than fifty rows** — past any
plausible window height, so beyond that there is no empty space to take up
and nothing to gain by risking it. The visible benefit is kept for the short
channels it was for, and large channels get the plain `Fill` that ListView
is designed around.

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
