# Mattermost protocol notes

Facts a from-scratch client must get right, confirmed against the Mattermost
server source at **`release-12.1`** unless marked otherwise. Citations are
paths within `mattermost/mattermost`.

These are recorded here rather than left in commit messages because most of
them are not in the published API documentation, and several contradict it.

> **Two documentation warnings.** `https://developers.mattermost.com/integrate/websocket/`
> is **gone (404)**; the WebSocket documentation is now a section inside
> `api/v4/source/introduction.yaml` in the server repo. And that same
> `introduction.yaml` still documents the `channel_viewed` event, which no
> longer exists — see below.

## Authentication

`POST /api/v4/users/login` — `server/channels/api4/user.go:2127`.

- The body is parsed with `model.MapFromJSON`, i.e. a **flat
  `map[string]string`**. Every value must be a JSON string, booleans included:
  `ldap_only` is compared as `props["ldap_only"] == "true"`.
- **The MFA code's field name is `token`**, not `mfa_token`.
- **The session token comes back in the `Token` response header**, not the body
  (`server/channels/app/login.go:228`). The body is the sanitized `User`.
- Session cookies are only set when the request sends
  `X-Requested-With: XMLHttpRequest`. A native client should not send it.
- Rate-limited to **5/s, burst 10**, hard-coded independent of config
  (`api4/user.go:69`).

Token use — `server/channels/app/authentication.go:493`:

1. **Cookie `MMAUTHTOKEN` is checked *first*, before the header.** So do not
   carry a cookie jar: a stray cookie silently wins over your `Authorization`.
2. `Authorization: Bearer <token>` (case-insensitive on `BEARER`)
3. `Authorization: Token <token>` — the OAuth form
4. `?access_token=` query string

Personal access tokens are identical in use and never expire until revoked.
They need `ServiceSettings.EnableUserAccessTokens` plus the
`create_user_access_token` permission. **This is the v1 auth path**, and the
only one available to an SSO-only user on a server where that setting is off —
which the login dialog must detect and say, rather than failing at the password
field.

### SSO without a webview: the desktop-token flow

Officially supported, and **not** a custom URL scheme:

1. The client generates its own opaque `client_token`.
2. It opens the **system browser** at
   `{SiteURL}/oauth/{service}/login?desktop_token={client_token}`
   (`server/channels/web/oauth.go:44`, read at `:476`) or
   `{SiteURL}/login/sso/saml?desktop_token={client_token}`
   (`web/saml.go:85`, round-tripped in a signed `RelayState`).
3. On success the server mints a `server_token` and redirects to
   `{SiteURL}/login/desktop?client_token=…&server_token=…`
   (`web/oauth.go:390`, `web/saml.go:227`).
4. The client exchanges it: **`POST /api/v4/users/login/desktop_token`** with
   `{"token": "<server_token>"}` → session token in the `Token` header
   (`api4/user.go:2302`).

`model.DesktopTokenTTL` is **3 minutes** (`server/public/model/user.go:73`), the
route is rate-limited to 2/s burst 1, and the exchange is rejected for users
who are not OAuth or SAML users.

**Unverified:** how the `/login/desktop` page hands `server_token` to a native
process. The server side is confirmed; the webapp route is not. Until that is
answered against a real server, v1 ships PAT and password/MFA only.

The custom-scheme flow (`/oauth/{service}/mobile_login`) is the **mobile** path.
It validates `redirect_to` against the admin-configured
`NativeAppSettings.AppCustomURLSchemes` and returns the token **in URL query
parameters** (`web/oauth.go:443`). `POST /api/v4/users/login/sso/code-exchange`
is deprecated, returns **410 Gone** when its feature flag is off, and its own
doc comment says not to use it.

## WebSocket

`GET {ws|wss}://host/api/v4/websocket` — `api4/websocket.go:52`, registered with
`APIHandlerTrustRequester`, so **the origin is checked**.

Reliable-reconnect query parameters (`api4/websocket.go:18`): `connection_id`,
`sequence_number`, `posted_ack`, and `disconnect_err_code` (1000–1016, or the
custom **4000** = client ping timeout, **4001** = sequence mismatch).

Auth is by header, cookie, or the `authentication_challenge` action. From
`app/platform/websocket_router.go`:

- **`seq` must be ≥ 1** — `if r.Seq <= 0` is rejected.
- **A bad or missing token closes the socket with no error frame.** There is no
  `FAIL` response; plan for "socket closed" as the auth-failure signal.
- The challenge is a silent no-op if the connection is already authenticated.

Keepalive — `app/platform/web_conn.go:32`:

| | |
|---|---|
| `pingInterval` | **60 s** — the server pings |
| `pongWaitTime` | **100 s** — it closes if no pong arrives |
| `writeWaitTime` | 30 s |
| max frame | 8192 bytes (`model.SocketMaxMessageSizeKb`) |

**The client only responds to pings; it does not send them.** Confirm the Rust
WebSocket crate answers PING automatically.

### Two different `seq` counters

Server→client **events** carry a `seq` that is a server-side per-connection
counter. Client→server **requests** carry a `seq` you choose, echoed back as
`seq_reply`. They share a name and nothing else, so `mm-api` calls them
`event_seq` and `client_seq` and never `seq`.

Disambiguating a frame: decode as an event first, and if `event != ""` it is
one; otherwise decode as a response and check `status != ""`. You cannot key on
a field being absent.

### Reconnect and gap detection

The server keeps a **128-event dead queue** per connection
(`web_conn.go:42`). On reconnect with `connection_id` + `sequence_number`:

- sequence found → it drains the missed events to you; no `hello`.
- sequence lost → it **mints a new `connection_id`, resets the counter and
  sends a fresh `hello`** (`web_conn.go:536`).

**Receiving a `hello` on a reconnect is the signal that you lost events and
must resync over REST.** `hello.server_version` is a 4-part composite,
`version.build.clientConfigHash.isLicensed` — when segment 3 changes, re-fetch
the client config.

### Payload fields that are JSON-encoded strings

The classic client bug. These arrive as **a string containing JSON**, not as a
nested object, and must be parsed twice:

| Event(s) | Field |
|---|---|
| `posted`, `post_edited`, `post_deleted`, `ephemeral_message` | `post` |
| `channel_updated` | `channel` |
| `channel_member_updated` | `channelMember` (note camelCase) |
| `thread_updated`, and `thread` on `posted` | `thread` |
| `reaction_added`, `reaction_removed` | `reaction` |
| `preferences_changed`, `preferences_deleted` | `preferences` |
| `emoji_added` | `emoji` |
| `group_added` | `teammate_ids` |

The exceptions: **`user` on `user_updated` is a nested object**, and
**`channel_times` on `multiple_channels_viewed` is a nested map**.

Also note `channel_created` carries only `channel_id` and `team_id` — **no
channel object**, so it must be followed by a fetch. And `typing` carries
`parent_id`, not `root_id`.

### `channel_viewed` does not exist

Renamed to `multiple_channels_viewed` in **v8.2**, and it now fires only for
channels that actually have unread messages. From the upgrade notes
(`docs/main/administration-guide/upgrade/important-upgrade-notes.mdx:905`):
*"The `channel_viewed` websocket event was changed to
`multiple_channels_viewed`"*. The published API docs still list the old name.
Handle both only if you must support ≤ 8.1.

`thread_read_changed` has **three different payload shapes** across its call
sites, one of which publishes **no data at all** (`app/user.go:3119`). Treat
every field as optional.

## Unread and read state

Confirmed from the server's own SQL — `GetChannelUnread`,
`server/channels/store/sqlstore/channel_store.go:928`:

```
unread messages      = Channels.TotalMsgCount     - ChannelMembers.MsgCount
unread root messages = Channels.TotalMsgCountRoot - ChannelMembers.MsgCountRoot
mentions             = ChannelMembers.MentionCount        (stored, not derived)
root mentions        = ChannelMembers.MentionCountRoot
```

Three consequences, implemented in `mm-core::unread`:

1. **Clamp at zero.** `UpdateLastViewedAt` assigns
   `greatest(cm.MsgCount, c.TotalMsgCount)` (`channel_store.go:2875`), so a
   member counter can legitimately sit above the channel total until the next
   channel update lands. Without the clamp that window renders negative.
2. **`notify_props.mark_unread == "mention"` *is* the muted state** — there is
   no separate flag (`model.ChannelMember.IsChannelMuted`). Muting suppresses
   the unread marker only; **a muted channel still shows a mention badge.**
3. **With CRT on, replies do not make a channel unread.** That is the entire
   reason the `*_root` fields exist.

CRT is a two-level decision: the server's `ServiceSettings.CollapsedThreads`
(`disabled` | `default_on` | `default_off` | `always_on`; **default
`always_on` as of 12.1**, historically `disabled`) combined with the user's
`display.collapsed_reply_threads` preference. **Never hardcode it.**

`last_viewed_at` and `last_update_at` are **omitted entirely** when reading
another user's membership — a `-1` sentinel plus `omitempty` — so both must be
`Option<i64>`.

### Marking read

`POST /api/v4/channels/members/{user_id}/view` (`api4/channel.go:2184`), body
`{channel_id, prev_channel_id, collapsed_threads_supported}`.

- **An empty `channel_id` is legal** and means focus loss / initial view.
- **`collapsed_threads_supported` must match your client**, or the server
  maintains the wrong counters.
- Response: `{"status":"OK","last_viewed_at_times":{...}}`.
- Emits `multiple_channels_viewed`.

Mark-unread-from-a-post is `POST /api/v4/users/{uid}/posts/{pid}/set_unread`,
whose body is parsed with `MapBoolFromJSON` and therefore takes **native JSON
booleans** — unlike login. For opening a channel at the new-messages line there
is a purpose-built endpoint: `GET /api/v4/users/{uid}/channels/{cid}/posts/unread`.

### Threads: two types, different field names

| | `ThreadMembership` | `ThreadResponse` |
|---|---|---|
| following | `following` | **absent** |
| viewed at | `last_view_at` | `last_viewed_at` |
| unread replies | **absent** | `unread_replies` |
| unread mentions | `unread_mentions` | `unread_mentions` |

Modelling these as one struct yields silent nulls. Thread metadata **does not
exist until the first reply** to a root post.

## Fetching posts

`GET /api/v4/channels/{channel_id}/posts` — the `since` / `after` / `before` /
`page` branches are **mutually exclusive and evaluated in that order**
(`api4/post.go:331`); extra parameters are silently ignored. `per_page` maxes at
**200** and is silently truncated above that; the default is 60.

`PostList` is `{order: [...], posts: {...}, prev_post_id, next_post_id,
has_next, first_inaccessible_post_time}`. **`posts` can contain entries not
listed in `order`** — iterate `order` for the timeline and treat `posts` as a
lookup map.

### `?since=` is the wrong primitive for reconnect backfill

From `GetPostsSince`, `store/sqlstore/post_store.go:1466`. None of this is
documented:

- **Hard `LIMIT 1000`, with no cursor and no overflow signal.** More than 1000
  posts changed in that channel and you **silently lose data**.
- **No ETag** — the `since` branch is the only one that skips `HandleEtag`.
- It filters on `UpdateAt` but orders by `CreateAt DESC`, so an old post edited
  just now appears deep in the list, not at the top.
- It has **no `DeleteAt = 0` filter**, so it returns soft-deleted posts. Check
  `delete_at` on every post and tombstone it.
- A UNION pulls in the root posts of changed replies, which is the mechanism
  behind `posts` ⊋ `order`.
- The comparison is strictly `>`, and many posts share a millisecond.

So: **`after={newest known post id}` with `per_page` paging and `next_post_id`
to walk** for new posts — ETag-cacheable and properly paginated — and `since`
*only* to catch edits and deletes to posts already held. A correct client needs
both.

Deletes are soft and **cascade to replies in one statement**
(`post_store.go:999`: `WHERE Id = $4 OR RootId = $4`), bumping `UpdateAt` and
recording `props.delete_by`.

## Rate limits, ETags, client config

**Rate limiting is off by default** (`RateLimitSettings.Enable = false`,
`model/config.go:2389`), but the login routes are always limited. On 429 the
body is the **plain text `limit exceeded`, not JSON** (`app/ratelimit.go:90`) —
the error path must not assume a JSON body. `X-Ratelimit-Reset` is a **delta in
seconds** despite the documentation calling it epoch seconds.

ETags (`web/context.go:230`) are compared by **exact string equality**, are
emitted **unquoted** — echo the bytes, never add quotes — and carry the server
version as their prefix, so **every ETag invalidates on server upgrade**. Do not
persist them across one. In 12.1 they exist on the posts page/before/after
branches (not `since`), `posts/unread`, `posts/{id}`, `posts/{id}/thread`,
`posts/{id}/files/info`, the four `GET /users/…` forms, profile image, users
in/not-in team, `channels/{id}/pinned`, the user's channels-in-team, and team
icon. **`GET /channels/{id}` has none.**

`GET /api/v4/config/client?format=old` returns **`map[string]string` — every
value is a string**, so `MaxFileSize` is `"104857600"`. **Unauthenticated
callers get a reduced map**, so fetch it after login. `format=old` is ignored by
12.1 but every first-party client sends it. Keys that matter:
`CollapsedThreads`, `EnableCustomEmoji`, `EnableFileAttachments`, `MaxFileSize`,
`EnableUserTypingMessages`, `TimeBetweenUserTypingUpdatesMilliseconds` (the
throttle for your own `user_typing` sends), `PostEditTimeLimit` (`-1` =
unlimited), `EnableLinkPreviews`.

Liveness: `GET /api/v4/system/ping?get_server_status=true` adds
`database_status` and `filestore_status`. **Plain `/ping` returns `OK` while the
database is unreachable**, which makes it the wrong readiness probe for the lab.

Prefer **capability probing over version comparison**: set a capability from
whether a field was actually present in a real payload, not from a version
string. Every hardcoded threshold gets a parity-matrix row.

## Running a server for the lab

The official compose setup is the `mattermost/docker` repo. The server image's
final stage is `gcr.io/distroless/base-debian12` with `USER mattermost` (uid
2000), so:

- **There is no shell and no curl in the container.** Every `exec` must be an
  absolute path to a real binary: `/mattermost/bin/mmctl`,
  `/mattermost/bin/mattermost`.
- The image bakes `MM_SERVICESETTINGS_ENABLELOCALMODE=true`, so **`mmctl
  --local` administers the server over a unix socket with no credentials** —
  the most useful single fact for the lab. (The config *default* for
  `EnableLocalMode` is `false`; it is the Dockerfile ENV that enables it.)
- Volumes must be owned **uid/gid 2000** or the server will not start.
- `MM_SERVICESETTINGS_SITEURL` must match how the client reaches it, or
  WebSocket origin checks and permalinks break.

First admin, two ways:

- `mmctl --local user create --email … --username … --password … --system-admin
  --email-verified --disable-welcome-email` (`cmd/mmctl/commands/user.go:370`).
  Works at any time; `--email-verified` is what makes it usable with no mail
  server.
- `POST /api/v4/users` unauthenticated, but **only while the server has zero
  non-bot users** — the first such user is auto-promoted to
  `system_admin system_user` (`app/users/users.go:50`), and the open-server
  check is bypassed for it (`app/user.go:286`).

Deterministic seeding: `mmctl --local sampledata --seed 42 --bulk out.jsonl`
(`cmd/mmctl/commands/sampledata.go:57`). **Commit the generated JSONL rather
than re-running the generator** — seed stability across server versions is
unconfirmed, and a committed fixture is byte-stable by construction.
