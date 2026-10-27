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

## channels

| feature | status | verified | notes |
|---|---|---|---|
| `channels.info_and_members` | partial | 12.0.1 | The channel's name in the header is a button, and it opens what the channel is and who is in it: the header line, then every member with their face, their presence and their name. Everything is already held -- the header came with the channel and the roster was fetched when it opened -- so it opens on the click rather than after a round trip. People who have left the server are left out, because the member count beside the channel's name leaves them out too and a list of thirty-five under a heading saying twenty-four is a window disagreeing with itself. A channel never opened says "Not loaded yet" rather than claiming it is deserted. Verified on a live account: Клиенты reads 24 in the header and 24 in the list, where the raw roster holds 35. NOT built: muting, notification preferences, adding or removing members, leaving, and the rest of Mattermost's channel menu. |

<details><summary><code>channels.info_and_members</code> wire</summary>

- `GET /api/v4/users`

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
| `posts.mention_autocomplete` | partial | 12.0.1 | Typing `@` offers the open channel's members under a CHANNEL MEMBERS heading and everybody else under NOT IN CHANNEL, with the name beside the username the way Mattermost shows it. Tab or Return completes, Up and Down move over the headings, Escape dismisses. In both composers, each knowing its own channel -- the thread composer takes it from the root post, not from whatever is open behind it. Ranked locally on four keys: the people in this conversation first, then match quality (username prefix, then human-name prefix, then a username merely containing it), then whoever spoke here most recently, and the alphabet only to break a tie. The roster is fetched once when the channel opens rather than per keystroke the way the web app does it; a channel never opened falls back to everyone known. Verified on a live account: `@` in Клиенты offers aragorn first, who had just posted, where it previously offered an alphabetical mix drawn from every channel visited. NOT built: there is no `/users/autocomplete` call, so somebody this client has never seen is typed out in full. `@all`, `@channel` and `@here` are offered under their own SPECIAL MENTIONS heading, from the same list the renderer highlights them with -- two lists would be a name the composer offers and the message does not mark. |
| `posts.warm_channels` | done | 12.0.1 | The thirty most recently active channels are fetched in the background once the sync is done, two at a time with a gap between starts, so that clicking one does not cost a round trip. Opening a channel draws what is already held first and splices in the answer when it comes, which is what removes the wait -- measured on a live account: 31 pages warmed within twenty seconds of launch, and a click then draws 95 rows in 99 ms with no request in the path. Bounded on purpose: a channel outside the thirty, or one silent for months, is still fetched the ordinary way. |
| `posts.edit` | partial | — | Edit message, on the reader's own messages, opening the markdown they wrote in a window of its own rather than turning the row into a text box: a recycled `ListView` row is taken out from under the caret the moment anything else arrives. NOT verified against a live server -- doing so means editing a real message in a real channel -- so what is confirmed is the request shape and the dialog, not the round trip. `PostEditTimeLimit` is not read either, so a message too old to edit is refused by the server rather than by the menu. |
| `posts.link_previews` | done | — | A URL the server has already resolved draws as a card under the message: the site name, the title as a link, the description, and the picture the page offered. Read out of `metadata.embeds`, where only the `opengraph` entries carry a `data` at all -- the others are `null`, and a card with neither title nor description is not drawn, because a card showing only a URL says less than the link already in the text. The picture is fetched straight from whoever hosts it, over https only and deliberately **without the session token**: sending a Mattermost credential to a third party because somebody pasted a URL would be a security bug. Asked once per URL and held in memory for the session; a dead image host leaves the card's text and no picture. |
| `posts.typing` | partial | — | "somebody is typing…" between the last message and the box being typed into, naming one person, two by name, or "Several people" beyond that. Held in the window rather than the store because it expires on a clock and the store is deliberately clock-free so that replaying events stays exact; an entry older than seven seconds is dropped, which is longer than the server's own five-second `TimeBetweenUserTypingUpdatesMilliseconds` throttle so the line does not flicker between a typist's keystrokes. NOT built: **sending** `user_typing`, so colleagues are not told when this client is typing. The socket loop only reads; announcing would mean splitting the stream so a writer can share it. |
| `posts.message_times` | done | — | The channel shows a clock time under a day separator, the way Mattermost does. The thread panel has no separators, so it says how long ago instead -- "6 days ago", "last week", "2 weeks ago" -- and falls back to a date past a month. Either way the whole moment, down to the second, is the tooltip on the time: two messages a moment apart show the same clock time, and that is where a reader goes to tell them apart. |
| `posts.composer` | partial | 12.0.1 | A `TextView` rather than an `Entry`, because a message is not one line: Shift+Return puts in a newline and Return sends, which is the binding Mattermost uses and the one muscle memory expects. An `Entry` could express neither, having no newline to insert. The box grows with what is typed and scrolls past about six lines rather than pushing the conversation off the top; a placeholder is drawn over it while it is empty, since a `TextView` has none of its own. The caret moves there when a channel is opened. Both composers, and the mention popup's Return still completes rather than sends, because its key handler is installed first and stops the event while the popup is up. Verified on a live account: three lines typed with Shift+Return stayed three lines, and Return cleared the box. NOT built: the formatting toolbar, the markdown preview eye, attachments and the emoji button. |
| `posts.spellcheck` | partial | 12.0.1 | Red squiggles under what is not a word, in **every language this machine has a dictionary for**, and replacements on a right-click. Hunspell dictionaries read with `spellbook`, which is pure Rust: no C library to link, nothing for the `.deb` but a recommendation, and no `unsafe` near a workspace that forbids it. (`spellbook` is MPL-2.0, which is file-level copyleft and imposes nothing on this MIT tree beyond its own files.) One dictionary per language rather than per locale -- the locale's variant wins, else the one whose region repeats the language -- so five English variants do not become five copies of English that accept both "colour" and "color". A word is wrong only when **no** dictionary knows it, which is what makes a sentence of mixed Russian and English check correctly in both. Deciding what counts as a word is pure and lives in `mm-md` at 100%: mentions, channel links, emoji codes, URLs, paths, snake_case, anything with a digit in it, acronyms, single letters and everything between backticks are all left alone, because a composer in a channel like this one would otherwise be solid red. The dictionaries load on a worker thread -- `ru_RU.dic` is 3.4 MB -- and checking is debounced 400 ms so a word is not marked wrong while it is still half typed. Verified on a live account: "Ошибки в сообщениии and teh wrong word" underlines exactly "сообщениии" and "teh". NOT built: a personal dictionary ("add to dictionary"), grammar as opposed to spelling, and spell-checking anything but the two composers. |
| `posts.search` | partial | 12.0.1 | Ctrl+Shift+F, or the magnifier in the header. The terms go to the server untouched: its own query language -- `from:`, `in:`, `before:`, quoted phrases, `-excluded` -- is parsed there, and a client that tried to interpret it could only get it wrong. Searched on Return rather than per keystroke, because every letter of a half-typed `from:legion` would otherwise be its own query. Results draw as the rows the Threads and Drafts views use -- who said it, where, when, the message -- since a hit is a place to go; clicking one opens its channel. The hits join the one post map but **not** a timeline: a result sits next to the result above it for reasons that have nothing to do with time, and filing them as history would be the client inventing adjacency the server never claimed. Verified on a live account: "deploy" answered with 100 hits across several channels. NOT built: paging past the first sixty, jumping to the hit's exact place in its channel's scrollback -- clicking a result opens the channel and that message's own thread beside it, which puts the message on screen without the timeline having to grow a segment around a post from a year ago -- and the match highlighting the server returns alongside the posts. |
| `posts.pinned_and_saved` | partial | 12.0.1 | The pin and bookmark buttons in the header: what is pinned in the open channel, and what this reader has saved anywhere. Both share the search page, because from the store's side the three are one thing -- a set of posts in an order the server chose, which is not the order of any channel. A failed fetch is reported rather than swallowed: an empty pane and a failed request look identical, and the drafts list already taught that lesson once. Saved posts are still `flagged` on the wire; the web app has said "Saved" since 5.x. Verified on a live account: three saved messages came back across three channels, and a channel with no pins said so. NOT built: unpinning or unsaving from these lists, and paging. |

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

<details><summary><code>posts.search</code> wire</summary>

- `POST /api/v4/teams/{team_id}/posts/search`

</details>

<details><summary><code>posts.pinned_and_saved</code> wire</summary>

- `GET /api/v4/channels/{channel_id}/pinned`
- `GET /api/v4/users/{user_id}/posts/flagged`

</details>

## search

| feature | status | verified | notes |
|---|---|---|---|
| `search.quick_switcher` | done | 12.0.1 | Ctrl+K, the way Mattermost binds it: type a few letters, press Return, be there. The one piece of searching that needs no endpoint -- every channel the reader is in is already in the store -- so it is ranked locally: a name that starts with what was typed, then one where a word does, then one that merely contains it, and unread first within each tier. Verified on a live account by typing `env` and landing in Prod-environment. |
| `search.people_and_channels` | planned | — | Message search is built -- see `posts.search`. What is left is searching for **people** and for **channels the reader is not already in**: the quick switcher only offers what is in the sidebar, so a channel you have not joined cannot be found at all without going back to the Electron client. |

## sidebar

| feature | status | verified | notes |
|---|---|---|---|
| `sidebar.hide_departed` | done | 12.0.1 | A one-to-one conversation with somebody who has left the server is kept out of the sidebar, and out of the quick switcher, which is drawn from the same list. Mentions never offered them. Verified on a live account: 7 of 27 direct messages were with deactivated people -- royo, madrunner, aiksandr, blossom, bozon, insmurfi, yachaha -- and all seven left, taking the HR category with them, since blossom was its only member. Hidden only when the roster actually says so: somebody this client has not fetched is shown, because "not known" must never be drawn as "gone". Their posts still render wherever they appear. NOTE: with no message search yet, a hidden conversation cannot be reopened from this client at all. |
| `sidebar.categories` | done | 12.0.1 | The server's grouping and ordering, including Favorites, Direct Messages and the user's own categories, with the stacking taken from `order` rather than from the array. Folding a category is written back: verified on a live server by folding one from the client and reading the flag back, false then true then false. A folded category still shows its unread channels and whichever channel is open, which is what folding means in Mattermost. An empty `sorting` -- what the server sends for a category nobody has reordered -- is drawn alphabetically, which is inferred from the web app rather than read from the server. |
| `sidebar.drag_between_categories` | partial | 12.0.1 | A channel or DM can be dragged from one category to another, and to a different place within one. Dropping it on a heading puts it at the top of that category, which is what a folded category needs: it shows no rows to aim between. Dropping it on a channel puts it just above that channel. Both categories go back in **one** request -- the bulk endpoint the web app's own drag-and-drop uses -- because sending them separately leaves a window in which the server has the channel in both categories or in neither, and the next sync reads that back as the truth. Applied locally first, like folding, so the sidebar follows the pointer rather than the round trip. Where it lands is expressed against a neighbouring channel rather than as an index, because the sidebar can be showing fewer rows than the category holds -- a folded category shows none, the unread filter hides the rest -- so a position on screen is not a position in `channel_ids`. Verified on a live account: a DM dragged out of QA onto the ALLERT heading moved, and dragging it back above quant restored the sidebar exactly. NOT built: dragging a category heading to reorder the categories themselves, and dragging a channel out of the sidebar. |

<details><summary><code>sidebar.categories</code> wire</summary>

- `GET /api/v4/users/{user_id}/teams/{team_id}/channels/categories`
- `PUT /api/v4/users/{user_id}/teams/{team_id}/channels/categories/{category_id}`

</details>

<details><summary><code>sidebar.drag_between_categories</code> wire</summary>

- `PUT /api/v4/users/{user_id}/teams/{team_id}/channels/categories`

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
| `users.profile_card` | partial | 12.0.1 | Clicking a name or a face in a message opens that person's card: their picture with its presence dot, their full name, their username, their job title, their email where the server discloses it, and the time where they are. Built from the roster already held rather than fetched, so it opens without a round trip; a field the server withheld is left out rather than drawn blank, which is why `email` is simply absent on a server that sanitizes it. The local time needs the person's IANA zone -- Mattermost sends it as three strings with the boolean written out as text -- and the line is omitted when there is no zone or this build's timezone database has never heard of it, because a missing clock beats a wrong one. Verified on a live account: aragorn's card reads his full name, @aragorn, Project Manager/Operations Assistant, and 18:20 at GMT+03:00. NOT built: the "Message" button that opens or creates a DM, custom statuses, and the card over a `@mention` in message text rather than over the author's name. |

<details><summary><code>users.presence</code> wire</summary>

- `POST /api/v4/users/status/ids`
- ws `status_change`

</details>

<details><summary><code>users.profile_card</code> wire</summary>

- `GET /api/v4/users`

</details>
