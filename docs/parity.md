# Parity

**Generated from `docs/parity.toml` by `cargo run -p xtask -- parity`.**
Edit that file, not this one; CI asserts they agree.

| status | meaning |
|---|---|
| `done` | implemented and verified against a live server |
| `partial` | implemented, with the gap named |
| `planned` | in scope, not built yet |
| `wontfix` | in scope for upstream, deliberately not for us |
| `blocked-no-webview` | impossible without a renderer we do not have |

A blank **verified** column means nobody has confirmed it against a
real server yet.

## auth

| feature | status | verified | notes |
|---|---|---|---|
| `auth.password_and_pat` | done | 12.0.1 | The session token arrives in the Token response header, 26 characters, with the body carrying only the sanitized user -- confirmed on a live server. The MFA field is named `token`, not `mfa_token`. A personal access token is used identically via Authorization: Bearer. |
| `auth.sso_desktop_token` | planned | — | The server side of the desktop-token flow is confirmed from source, including the 3-minute TTL. What is NOT confirmed is how the /login/desktop page hands server_token to a native process. Until that is answered, SSO users use a personal access token, and the login dialog should say so rather than failing at the password field. |

<details><summary><code>auth.password_and_pat</code> wire</summary>

- `POST /api/v4/users/login`
- `GET /api/v4/users/me`

</details>

<details><summary><code>auth.sso_desktop_token</code> wire</summary>

- `POST /api/v4/users/login/desktop_token`

</details>

## desktop

| feature | status | verified | notes |
|---|---|---|---|
| `calls` | blocked-no-webview | — | Ships as webapp code and WebRTC; requires a renderer we do not have. Fallback: `OpenInBrowser({server}/{team}/channels/{channel})` |
| `plugins` | blocked-no-webview | — | Plugin UI is server-served web app code, by design. Fallback: `OpenInBrowser({server}/{team}/channels/{channel})` |
| `boards` | blocked-no-webview | — | Focalboard ships as a webapp plugin. Fallback: `OpenInBrowser({server}/boards)` |
| `playbooks` | blocked-no-webview | — | Playbooks ships as a webapp plugin. Fallback: `OpenInBrowser({server}/playbooks)` |
| `desktop.tray` | planned | GNOME Shell 46.0 / Ubuntu 24.04 / X11 | De-risked ahead of P3: ubuntu-appindicators is enabled by default, the StatusNotifierWatcher is present with a registered host, and a real ksni item registers successfully. See docs/gtk-notes.md. Depends on that GNOME extension, so the implementation must detect an absent watcher and say so rather than publishing into the void. |

## drafts

| feature | status | verified | notes |
|---|---|---|---|
| `drafts.view` | partial | 12.0.1 | Drafts are server-side, so what was typed in the web app is listed here, and choosing one goes to its channel. Verified against a live account holding 11 of them. NOT built: this client writes no drafts of its own, so a message half-typed here is lost on switching channels -- the composer's text is still widget state rather than a draft in the store. |

<details><summary><code>drafts.view</code> wire</summary>

- `GET /api/v4/users/{user_id}/teams/{team_id}/drafts`

</details>

## files

| feature | status | verified | notes |
|---|---|---|---|
| `files.inline_images` | partial | 12.0.1 | An attached image is drawn in the row at the size its metadata states, from the `mini_preview` thumbnail that rides in the post so the row never reflows, and clicking it opens the full-size copy in a window. NOT built: upload, download to disk, and any non-image attachment, which shows as a line of text. An animated GIF renders its first frame. |

<details><summary><code>files.inline_images</code> wire</summary>

- `GET /api/v4/files/{file_id}`
- `GET /api/v4/files/{file_id}/preview`

</details>

## notify

| feature | status | verified | notes |
|---|---|---|---|
| `desktop.notifications` | partial | — | Policy is implemented and fully covered in mm-desktop; delivery goes through zbus in mm-desktop-xdg and returns Ok against gnome-shell 46. Whether a banner is actually drawn is the shell's own policy and cannot be asserted from here -- `mattermost-rs doctor` exists so a person can settle it in one command. UNVERIFIED: whether muting alone should silence notifications; see NotifySettings::muted_is_silent. |

## posts

| feature | status | verified | notes |
|---|---|---|---|
| `apps_framework` | blocked-no-webview | — | Interactive message buttons and modals are rendered by the web app. Fallback: `OpenInBrowser permalink to the post` |
| `ws.live_events` | partial | 12.0.1 | posted / post_edited / post_deleted confirmed live, including the `post` payload being a JSON document inside a JSON string. A hello carrying a NEW connection id is treated as lost events; an event-sequence hole is treated the same way. NOT yet done: acting on either signal with a REST resync -- the GUI currently re-syncs the sidebar but does not close a post gap, which is why mm-core::Timeline refuses to merge a page whose adjacency it cannot establish. |
| `posts.timeline_gaps` | partial | — | The timeline holds one contiguous run and REFUSES to merge a page whose adjacency cannot be established, rather than joining two disjoint runs and letting the reader scroll past messages with nothing on screen to say so. The plan's full multi-segment model is not built, which is what `since`-based backfill needs. Until then every fetch is anchored to what is held. |
| `posts.reactions` | done | 12.0.1 | Reactions on the message and one-click reactions on the hover toolbar, both toggles: there is no separate way to take a reaction back, here or anywhere in Mattermost. The one-click buttons are ranked by what this reader uses most. Custom emoji are NOT fetched, so one resolves to its shortcode rather than an image. |
| `posts.mention_autocomplete` | partial | 12.0.1 | Typing `@` offers the open channel's members under a CHANNEL MEMBERS heading and everybody else under NOT IN CHANNEL, with the name beside the username the way Mattermost shows it. Tab or Return completes, Up and Down move over the headings, Escape dismisses. In both composers, each knowing its own channel -- the thread composer takes it from the root post, not from whatever is open behind it. Ranked locally on four keys: the people in this conversation first, then match quality (username prefix, then human-name prefix, then a username merely containing it), then whoever spoke here most recently, and the alphabet only to break a tie. The roster is fetched once when the channel opens rather than per keystroke the way the web app does it; a channel never opened falls back to everyone known. Verified on a live account: `@` in Клиенты offers aragorn first, who had just posted, where it previously offered an alphabetical mix drawn from every channel visited. NOT built: `@all`, `@channel` and `@here`, and there is no `/users/autocomplete` call, so somebody this client has never seen is typed out in full. |
| `posts.warm_channels` | done | 12.0.1 | The thirty most recently active channels are fetched in the background once the sync is done, two at a time with a gap between starts, so that clicking one does not cost a round trip. Opening a channel draws what is already held first and splices in the answer when it comes, which is what removes the wait -- measured on a live account: 31 pages warmed within twenty seconds of launch, and a click then draws 95 rows in 99 ms with no request in the path. Bounded on purpose: a channel outside the thirty, or one silent for months, is still fetched the ordinary way. |
| `posts.edit` | partial | — | Edit message, on the reader's own messages, opening the markdown they wrote in a window of its own rather than turning the row into a text box: a recycled `ListView` row is taken out from under the caret the moment anything else arrives. NOT verified against a live server -- doing so means editing a real message in a real channel -- so what is confirmed is the request shape and the dialog, not the round trip. `PostEditTimeLimit` is not read either, so a message too old to edit is refused by the server rather than by the menu. |
| `posts.link_previews` | done | — | A URL the server has already resolved draws as a card under the message: the site name, the title as a link, the description, and the picture the page offered. Read out of `metadata.embeds`, where only the `opengraph` entries carry a `data` at all -- the others are `null`, and a card with neither title nor description is not drawn, because a card showing only a URL says less than the link already in the text. The picture is fetched straight from whoever hosts it, over https only and deliberately **without the session token**: sending a Mattermost credential to a third party because somebody pasted a URL would be a security bug. Asked once per URL and held in memory for the session; a dead image host leaves the card's text and no picture. |
| `posts.typing` | partial | — | "somebody is typing…" between the last message and the box being typed into, naming one person, two by name, or "Several people" beyond that. Held in the window rather than the store because it expires on a clock and the store is deliberately clock-free so that replaying events stays exact; an entry older than seven seconds is dropped, which is longer than the server's own five-second `TimeBetweenUserTypingUpdatesMilliseconds` throttle so the line does not flicker between a typist's keystrokes. NOT built: **sending** `user_typing`, so colleagues are not told when this client is typing. The socket loop only reads; announcing would mean splitting the stream so a writer can share it. |
| `posts.message_times` | done | — | The channel shows a clock time under a day separator, the way Mattermost does. The thread panel has no separators, so it says how long ago instead -- "6 days ago", "last week", "2 weeks ago" -- and falls back to a date past a month. Either way the whole moment, down to the second, is the tooltip on the time: two messages a moment apart show the same clock time, and that is where a reader goes to tell them apart. |
| `posts.composer` | partial | 12.0.1 | A `TextView` rather than an `Entry`, because a message is not one line: Shift+Return puts in a newline and Return sends, which is the binding Mattermost uses and the one muscle memory expects. An `Entry` could express neither, having no newline to insert. The box grows with what is typed and scrolls past about six lines rather than pushing the conversation off the top; a placeholder is drawn over it while it is empty, since a `TextView` has none of its own. The caret moves there when a channel is opened. Both composers, and the mention popup's Return still completes rather than sends, because its key handler is installed first and stops the event while the popup is up. Verified on a live account: three lines typed with Shift+Return stayed three lines, and Return cleared the box. NOT built: the formatting toolbar, the markdown preview eye, attachments and the emoji button. |

<details><summary><code>ws.live_events</code> wire</summary>

- ws `hello`
- ws `posted`
- ws `post_edited`
- ws `post_deleted`
- ws `channel_member_updated`
- ws `multiple_channels_viewed`
- ws `channel_viewed`
- ws `typing`
- ws `status_change`
- ws `user_updated`

</details>

<details><summary><code>posts.reactions</code> wire</summary>

- `POST /api/v4/reactions`
- `DELETE /api/v4/users/{user_id}/posts/{post_id}/reactions/{emoji_name}`
- ws `reaction_added`
- ws `reaction_removed`

</details>

<details><summary><code>posts.mention_autocomplete</code> wire</summary>

- `GET /api/v4/users`

</details>

<details><summary><code>posts.warm_channels</code> wire</summary>

- `GET /api/v4/channels/{channel_id}/posts`

</details>

<details><summary><code>posts.edit</code> wire</summary>

- `PUT /api/v4/posts/{post_id}`
- ws `post_edited`

</details>

<details><summary><code>posts.link_previews</code> wire</summary>

- `GET /api/v4/channels/{channel_id}/posts`

</details>

<details><summary><code>posts.typing</code> wire</summary>

- ws `typing`

</details>

<details><summary><code>posts.composer</code> wire</summary>

- `POST /api/v4/posts`

</details>

## search

| feature | status | verified | notes |
|---|---|---|---|
| `search.quick_switcher` | done | 12.0.1 | Ctrl+K, the way Mattermost binds it: type a few letters, press Return, be there. The one piece of searching that needs no endpoint -- every channel the reader is in is already in the store -- so it is ranked locally: a name that starts with what was typed, then one where a word does, then one that merely contains it, and unread first within each tier. Verified on a live account by typing `env` and landing in Prod-environment. |
| `search.messages` | planned | — | No message search, and no searching for people or for channels the reader is not already in. The largest remaining gap against the Electron client, and the one most likely to send a reader back to it. |

## sidebar

| feature | status | verified | notes |
|---|---|---|---|
| `sidebar.hide_departed` | done | 12.0.1 | A one-to-one conversation with somebody who has left the server is kept out of the sidebar, and out of the quick switcher, which is drawn from the same list. Mentions never offered them. Verified on a live account: 7 of 27 direct messages were with deactivated people -- royo, madrunner, aiksandr, blossom, bozon, insmurfi, yachaha -- and all seven left, taking the HR category with them, since blossom was its only member. Hidden only when the roster actually says so: somebody this client has not fetched is shown, because "not known" must never be drawn as "gone". Their posts still render wherever they appear. NOTE: with no message search yet, a hidden conversation cannot be reopened from this client at all. |
| `sidebar.categories` | done | 12.0.1 | The server's grouping and ordering, including Favorites, Direct Messages and the user's own categories, with the stacking taken from `order` rather than from the array. Folding a category is written back: verified on a live server by folding one from the client and reading the flag back, false then true then false. A folded category still shows its unread channels and whichever channel is open, which is what folding means in Mattermost. An empty `sorting` -- what the server sends for a category nobody has reordered -- is drawn alphabetically, which is inferred from the web app rather than read from the server. |

<details><summary><code>sidebar.categories</code> wire</summary>

- `GET /api/v4/users/{user_id}/teams/{team_id}/channels/categories`
- `PUT /api/v4/users/{user_id}/teams/{team_id}/channels/categories/{category_id}`

</details>

## threads

| feature | status | verified | notes |
|---|---|---|---|
| `threads.view` | partial | 12.0.1 | The followed-thread list, with the root message, the channel, the participants' faces and the reply count, and Followed/Unreads tabs. The totals behind the sidebar badge are the server's account-wide ones, not a count of the page: confirmed on a live server reporting 961 threads while returning 50. NOT built: the filter control beside the tabs, and following or unfollowing a thread from here. |

<details><summary><code>threads.view</code> wire</summary>

- `GET /api/v4/users/{user_id}/teams/{team_id}/threads`

</details>

## unread

| feature | status | verified | notes |
|---|---|---|---|
| `unread.channel_counts` | done | 12.0.1 | Verified against a live Mattermost 12.0.1: a channel with 6 plain messages and 1 mention reported 7 unread with a mention badge, while a muted channel reported its 3 unread without an unread marker and contributed nothing to the mention total. CollapsedThreads came back as always_on and the client switched itself to root-only counting. Still NOT verified at the edges: a muted channel holding a direct mention, DM channels, and the interaction with ignore_channel_mentions. That policy lives in the webapp's TypeScript selectors and wants a recorded trace rather than more reading. |

<details><summary><code>unread.channel_counts</code> wire</summary>

- `GET /api/v4/users/me/teams/{team_id}/channels/members`
- `POST /api/v4/channels/members/me/view`
- ws `channel_member_updated`
- ws `multiple_channels_viewed`

</details>

## users

| feature | status | verified | notes |
|---|---|---|---|
| `users.presence` | partial | 12.0.1 | A dot in the corner of every face: green for online, amber for away, red for do not disturb, and nothing at all for offline, which is what Mattermost does rather than draw a column of grey dots about people who are simply not at work. Fetched in one POST for the sidebar's DM partners after a sync and for a channel's roster after it opens -- the ids go in the body because a roster is two hundred of them, which is past the usual proxy limit and would put everyone the reader is looking at into an access log. Kept current by `status_change`, and a repeat of what is already held produces no change so a second device reporting the same state does not redraw every row. An unrecognised status reads as offline rather than failing the batch. Verified on a live account: aragorn's face carries a green dot. NOT built: setting your own status, and there is no periodic re-poll -- a person who goes away while nothing else happens keeps their old dot until the server says otherwise. |

<details><summary><code>users.presence</code> wire</summary>

- `POST /api/v4/users/status/ids`
- ws `status_change`

</details>
