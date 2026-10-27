//! Request builders and response parsers for the REST API.
//!
//! Every function here is pure. Builders return an [`HttpRequest`]; parsers
//! take an [`HttpResponse`] and return a model or an [`ApiError`].

use crate::error::ApiError;
use crate::http::{HttpRequest, HttpResponse};
use mm_types::{
    CategoryList, Channel, ChannelCategory, ChannelId, ChannelMember, ChannelStats, CustomEmoji,
    Draft, EmojiId, FileId, FileRef, Post, PostId, PostList, Preference, Reaction, Status, Team,
    TeamId, ThreadList, User, UserId,
};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Everything lives under this prefix.
pub const API: &str = "/api/v4";

/// The server caps `per_page` at 200 and silently truncates anything larger,
/// so asking for more is a way to think you have a full page when you do not.
pub const MAX_PER_PAGE: u32 = 200;

/// The server's default when `per_page` is omitted.
pub const DEFAULT_PER_PAGE: u32 = 60;

// ---------------------------------------------------------------- parsing

fn parse_json<T: for<'de> Deserialize<'de>>(
    resp: &HttpResponse,
    context: &'static str,
) -> Result<T, ApiError> {
    if !resp.is_success() {
        return Err(ApiError::from_response(resp));
    }
    serde_json::from_slice(&resp.body).map_err(|e| ApiError::malformed(context, e))
}

// ---------------------------------------------------------------- auth

/// Credentials for `POST /users/login`.
///
/// The request body is a flat `map[string]string` on the server side, so
/// **every value must be a JSON string**, booleans included. The multi-factor
/// code's field name is `token`, not `mfa_token`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Login {
    pub login_id: String,
    pub password: String,
    /// The six-digit multi-factor code, when the server has asked for one.
    pub mfa_token: Option<String>,
}

#[must_use]
pub fn login(creds: &Login) -> HttpRequest {
    let mut body = serde_json::Map::new();
    body.insert("login_id".into(), creds.login_id.clone().into());
    body.insert("password".into(), creds.password.clone().into());
    if let Some(code) = &creds.mfa_token {
        body.insert("token".into(), code.clone().into());
    }
    HttpRequest::post(format!("{API}/users/login")).json(&body.into())
}

/// A successful login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The session token, taken from the response **header**.
    pub token: String,
    pub user: User,
}

/// Parse a login response.
///
/// The session token arrives in the `Token` response header, not in the body —
/// the body is the sanitized user. Missing that is the single most common way
/// a first attempt at a Mattermost client fails, because the request appears
/// to succeed and nothing works afterwards.
pub fn parse_login(resp: &HttpResponse) -> Result<Session, ApiError> {
    let user: User = parse_json(resp, "POST /users/login")?;
    let token = resp.header("Token").ok_or_else(|| {
        ApiError::malformed(
            "POST /users/login",
            "the response carried no Token header; the session token is returned there, \
             not in the body",
        )
    })?;
    if token.is_empty() {
        return Err(ApiError::malformed(
            "POST /users/login",
            "the Token header was present but empty",
        ));
    }
    Ok(Session {
        token: token.to_owned(),
        user,
    })
}

#[must_use]
pub fn logout() -> HttpRequest {
    HttpRequest::post(format!("{API}/users/logout"))
}

#[must_use]
pub fn me() -> HttpRequest {
    HttpRequest::get(format!("{API}/users/me"))
}

pub fn parse_user(resp: &HttpResponse) -> Result<User, ApiError> {
    parse_json(resp, "GET /users/me")
}

// ---------------------------------------------------------------- system

#[must_use]
pub fn ping() -> HttpRequest {
    HttpRequest::get(format!("{API}/system/ping")).q("get_server_status", "true")
}

/// The ping response.
///
/// Plain `/ping` returns `OK` even while the database is unreachable, which is
/// why the builder always asks for the server status: `database_status` is the
/// field that makes this a real readiness probe.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Ping {
    pub status: String,
    pub database_status: String,
    pub filestore_status: String,
}

impl Ping {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.status == "OK"
    }

    /// Healthy *and* able to serve data. `database_status` is absent on
    /// servers that do not report it, so an empty value is not a failure.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.is_healthy() && (self.database_status.is_empty() || self.database_status == "OK")
    }
}

pub fn parse_ping(resp: &HttpResponse) -> Result<Ping, ApiError> {
    parse_json(resp, "GET /system/ping")
}

/// `GET /config/client?format=old`.
///
/// `format=old` is ignored by current servers but every first-party client
/// sends it, so it is sent here for compatibility in both directions.
#[must_use]
pub fn client_config() -> HttpRequest {
    HttpRequest::get(format!("{API}/config/client")).q("format", "old")
}

/// The client configuration.
///
/// **Every value is a string**, including numbers and booleans:
/// `MaxFileSize` arrives as `"104857600"`. An unauthenticated caller gets a
/// reduced map, so this must be fetched after login.
pub fn parse_client_config(resp: &HttpResponse) -> Result<BTreeMap<String, String>, ApiError> {
    parse_json(resp, "GET /config/client")
}

// ---------------------------------------------------------------- teams

#[must_use]
pub fn my_teams() -> HttpRequest {
    HttpRequest::get(format!("{API}/users/me/teams"))
}

pub fn parse_teams(resp: &HttpResponse) -> Result<Vec<Team>, ApiError> {
    parse_json(resp, "GET /users/me/teams")
}

// ---------------------------------------------------------------- channels

#[must_use]
pub fn my_channels(team: &TeamId) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/me/teams/{team}/channels"))
}

pub fn parse_channels(resp: &HttpResponse) -> Result<Vec<Channel>, ApiError> {
    parse_json(resp, "GET /users/me/teams/{team}/channels")
}

/// My membership of every channel I am in on this team — the unread source.
#[must_use]
pub fn my_channel_members(team: &TeamId) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/me/teams/{team}/channels/members"))
}

pub fn parse_channel_members(resp: &HttpResponse) -> Result<Vec<ChannelMember>, ApiError> {
    parse_json(resp, "GET /users/me/teams/{team}/channels/members")
}

/// The user's sidebar categories for a team.
///
/// Unlike most of the channel endpoints this one is addressed by explicit
/// user id -- there is no `/users/me/` form -- so the caller has to know who
/// it is before it can draw a sidebar.
#[must_use]
pub fn my_categories(user: &UserId, team: &TeamId) -> HttpRequest {
    HttpRequest::get(format!(
        "{API}/users/{user}/teams/{team}/channels/categories"
    ))
}

pub fn parse_categories(resp: &HttpResponse) -> Result<CategoryList, ApiError> {
    parse_json(resp, "GET /users/{user}/teams/{team}/channels/categories")
}

/// Replace one sidebar category.
///
/// How the server records a category being folded shut: `collapsed` is a
/// field on the category, and the whole category is sent back, because
/// the server replaces rather than patches. Addressed by explicit user id
/// like the fetch above -- there is no `/users/me/` form.
#[must_use]
pub fn update_category(user: &UserId, team: &TeamId, category: &ChannelCategory) -> HttpRequest {
    let body = serde_json::to_value(category).unwrap_or_default();
    HttpRequest::put(format!(
        "{API}/users/{user}/teams/{team}/channels/categories/{id}",
        id = category.id
    ))
    .json(&body)
}

pub fn parse_category(resp: &HttpResponse) -> Result<ChannelCategory, ApiError> {
    parse_json(
        resp,
        "PUT /users/{user}/teams/{team}/channels/categories/{id}",
    )
}

/// Replace several sidebar categories at once.
///
/// What moving a channel between categories needs: the one it left and
/// the one it joined have to change together, and sending them as two
/// requests leaves a window in which the server believes the channel is
/// in both or in neither. This is the endpoint the web app's own
/// drag-and-drop uses, for the same reason.
#[must_use]
pub fn update_categories(
    user: &UserId,
    team: &TeamId,
    categories: &[ChannelCategory],
) -> HttpRequest {
    let body = serde_json::to_value(categories).unwrap_or_default();
    HttpRequest::put(format!(
        "{API}/users/{user}/teams/{team}/channels/categories"
    ))
    .json(&body)
}

pub fn parse_updated_categories(resp: &HttpResponse) -> Result<Vec<ChannelCategory>, ApiError> {
    parse_json(resp, "PUT /users/{user}/teams/{team}/channels/categories")
}

/// Public channels in a team whose name or purpose matches.
///
/// The quick switcher can only offer what is already in the sidebar,
/// which means a channel the reader has not joined cannot be found at
/// all without going back to the web app. This is the endpoint that
/// fixes that: it searches the team's **open** channels, joined or not,
/// and the server decides what the reader is allowed to see.
#[must_use]
pub fn search_channels(team: &TeamId, wanted: &str) -> HttpRequest {
    HttpRequest::post(format!("{API}/teams/{team}/channels/search"))
        .json(&serde_json::json!({ "term": wanted }))
}

pub fn parse_channel_search(resp: &HttpResponse) -> Result<Vec<Channel>, ApiError> {
    parse_json(resp, "POST /teams/{team}/channels/search")
}

/// People in a team whose name matches.
///
/// `allow_inactive` is left false: somebody who has left the server
/// cannot be messaged, and the sidebar already hides them.
#[must_use]
pub fn search_users(team: &TeamId, wanted: &str, limit: u32) -> HttpRequest {
    HttpRequest::post(format!("{API}/users/search")).json(&serde_json::json!({
        "term": wanted,
        "team_id": team.as_str(),
        "allow_inactive": false,
        "limit": limit,
    }))
}

pub fn parse_user_search(resp: &HttpResponse) -> Result<Vec<User>, ApiError> {
    parse_json(resp, "POST /users/search")
}

/// Join a channel.
///
/// The answer is the new membership; the channel itself has to be
/// fetched separately, which `one_channel` is for -- a channel found by
/// search is not in the client's own list until it has been joined.
#[must_use]
pub fn join_channel(channel: &ChannelId, user: &UserId) -> HttpRequest {
    HttpRequest::post(format!("{API}/channels/{channel}/members"))
        .json(&serde_json::json!({ "user_id": user.as_str() }))
}

pub fn parse_channel_member(resp: &HttpResponse) -> Result<ChannelMember, ApiError> {
    parse_json(resp, "POST /channels/{channel}/members")
}

/// One channel by id. **No `ETag` on this route**, unlike most of the
/// others, so there is nothing to cache against.
#[must_use]
pub fn one_channel(channel: &ChannelId) -> HttpRequest {
    HttpRequest::get(format!("{API}/channels/{channel}"))
}

/// Open the one-to-one conversation between two people.
///
/// Creates it if it does not exist and returns the existing one if it
/// does, so there is nothing to check first. The body is a bare array
/// of two ids -- both of them, including your own.
#[must_use]
pub fn open_direct(me: &UserId, them: &UserId) -> HttpRequest {
    HttpRequest::post(format!("{API}/channels/direct"))
        .json(&serde_json::json!([me.as_str(), them.as_str()]))
}

pub fn parse_channel(resp: &HttpResponse) -> Result<Channel, ApiError> {
    parse_json(resp, "POST /channels/direct")
}

/// Follow a thread, or stop following it.
///
/// The thread's id *is* the root post's id. There is no `following`
/// field on what the server returns -- being in the followed list is
/// what following means -- so this is the only way to say it.
///
/// `PUT` to follow and `DELETE` to stop, which is the one place in
/// this API where the verb carries the whole meaning and the body is
/// empty.
#[must_use]
pub fn follow_thread(user: &UserId, team: &TeamId, root: &PostId, following: bool) -> HttpRequest {
    let path = format!("{API}/users/{user}/teams/{team}/threads/{root}/following");
    if following {
        HttpRequest::put(path)
    } else {
        HttpRequest::delete(path)
    }
}

/// Mute a channel, or let it speak again.
///
/// There is no mute flag: `notify_props.mark_unread` is `"mention"` for
/// a muted channel and `"all"` for one that is not, which is confirmed
/// from the server and is the thing most likely to be got wrong by
/// somebody looking for a boolean. The endpoint patches rather than
/// replaces, so the other notify props are left alone.
#[must_use]
pub fn set_channel_muted(channel: &ChannelId, user: &UserId, muted: bool) -> HttpRequest {
    let mark = if muted { "mention" } else { "all" };
    HttpRequest::put(format!(
        "{API}/channels/{channel}/members/{user}/notify_props"
    ))
    .json(&serde_json::json!({ "mark_unread": mark }))
}

/// Upload one file to a channel, before a post can refer to it.
///
/// Mattermost uploads first and posts second: the message carries
/// `file_ids`, and a file with no post attached to it is simply an
/// orphan the server cleans up. That is also why a failed post after a
/// successful upload is not a disaster.
///
/// The channel goes in the form rather than the path, which is what the
/// endpoint expects; `client_ids` is left out, since this client waits
/// for the answer rather than matching uploads up afterwards.
#[must_use]
pub fn upload_file(channel: &ChannelId, filename: &str, content: Vec<u8>) -> HttpRequest {
    HttpRequest::post(format!("{API}/files")).multipart(
        vec![("channel_id".to_owned(), channel.to_string())],
        "files",
        filename,
        content,
    )
}

/// What the server made of an upload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Uploaded {
    pub file_infos: Vec<FileRef>,
}

pub fn parse_upload(resp: &HttpResponse) -> Result<Uploaded, ApiError> {
    parse_json(resp, "POST /files")
}

/// Search a team's messages.
///
/// `is_or_search` false is Mattermost's default and means every term
/// must appear, which is what somebody typing two words expects. The
/// terms go through untouched: the server's own query language --
/// `from:`, `in:`, `before:`, quoted phrases, `-excluded` -- is parsed
/// there, and a client that tried to interpret it would only be able to
/// get it wrong.
///
/// `page` is zero-based, and the only way to reach anything past the
/// first `per_page` hits: search has no `before`/`after` cursor the way
/// a channel's history does.
#[must_use]
pub fn search_posts(team: &TeamId, terms: &str, page: u32, per_page: u32) -> HttpRequest {
    HttpRequest::post(format!("{API}/teams/{team}/posts/search")).json(&serde_json::json!({
        "terms": terms,
        "is_or_search": false,
        "page": page,
        "per_page": per_page,
    }))
}

pub fn parse_search(resp: &HttpResponse) -> Result<PostList, ApiError> {
    parse_json(resp, "POST /teams/{team}/posts/search")
}

/// The posts pinned in a channel.
#[must_use]
pub fn pinned_posts(channel: &ChannelId) -> HttpRequest {
    HttpRequest::get(format!("{API}/channels/{channel}/pinned"))
}

pub fn parse_pinned(resp: &HttpResponse) -> Result<PostList, ApiError> {
    parse_json(resp, "GET /channels/{channel}/pinned")
}

/// The posts this reader has saved. Mattermost still calls them
/// flagged on the wire; the web app has said "Saved" since 5.x.
#[must_use]
pub fn saved_posts(user: &UserId, per_page: u32) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/{user}/posts/flagged")).q("per_page", per_page)
}

pub fn parse_saved(resp: &HttpResponse) -> Result<PostList, ApiError> {
    parse_json(resp, "GET /users/{user}/posts/flagged")
}

/// The threads this reader follows, newest reply first.
///
/// `extended=false`: participants come back as objects carrying only an
/// id, which the roster already knows how to name, and asking for the
/// full users would fetch every participant of every thread on top of a
/// sync that has already fetched most of them.
///
/// The totals in the answer are the account's, not the page's, which is
/// what the sidebar badge wants.
#[must_use]
pub fn my_threads(user: &UserId, team: &TeamId, per_page: u32) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/{user}/teams/{team}/threads"))
        .q("extended", "false")
        .q("per_page", per_page)
}

pub fn parse_threads(resp: &HttpResponse) -> Result<ThreadList, ApiError> {
    parse_json(resp, "GET /users/{user}/teams/{team}/threads")
}

/// This reader's unsent messages for a team.
///
/// Server-side, so they follow the reader between clients: what the web
/// app has half-typed is what this lists.
#[must_use]
pub fn my_drafts(user: &UserId, team: &TeamId) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/{user}/teams/{team}/drafts"))
}

/// **The body is `null`, not `[]`, when there are no drafts.** Confirmed
/// against a live server: the same account returned an array while it
/// held drafts and `null` once it did not. Parsing straight into a
/// `Vec` therefore fails on the ordinary empty case, and the failure
/// looks exactly like an account with no drafts -- which is how it went
/// unnoticed until the Drafts row stopped appearing.
pub fn parse_drafts(resp: &HttpResponse) -> Result<Vec<Draft>, ApiError> {
    let drafts: Option<Vec<Draft>> = parse_json(resp, "GET /users/{user}/teams/{team}/drafts")?;
    Ok(drafts.unwrap_or_default())
}

/// How many people are in a channel, for the channel header.
#[must_use]
pub fn channel_stats(channel: &ChannelId) -> HttpRequest {
    HttpRequest::get(format!("{API}/channels/{channel}/stats"))
}

pub fn parse_channel_stats(resp: &HttpResponse) -> Result<ChannelStats, ApiError> {
    parse_json(resp, "GET /channels/{id}/stats")
}

/// React to a post.
///
/// The body carries the reacting user's id even though the session already
/// identifies them: the endpoint is also how an admin-level integration
/// reacts on someone else's behalf.
#[must_use]
pub fn add_reaction(user: &UserId, post: &PostId, emoji: &str) -> HttpRequest {
    let body = serde_json::json!({
        "user_id": user.as_str(),
        "post_id": post.as_str(),
        "emoji_name": emoji,
    });
    HttpRequest::post(format!("{API}/reactions")).json(&body)
}

pub fn parse_reaction(resp: &HttpResponse) -> Result<Reaction, ApiError> {
    parse_json(resp, "POST /reactions")
}

/// Take a reaction back. The emoji name is in the path, so it has to be
/// percent-safe; Mattermost's own validation limits it to alphanumerics,
/// `_`, `-` and `+`, none of which need escaping.
#[must_use]
pub fn remove_reaction(user: &UserId, post: &PostId, emoji: &str) -> HttpRequest {
    HttpRequest::delete(format!("{API}/users/{user}/posts/{post}/reactions/{emoji}"))
}

/// A user's avatar, as image bytes.
///
/// Every user has one whether or not they uploaded anything: the server
/// draws initials on a coloured square for those who did not, so there is no
/// "no avatar" case to handle.
#[must_use]
pub fn profile_image(user: &UserId) -> HttpRequest {
    HttpRequest::get(format!("{API}/users/{user}/image"))
}

/// An image response, which is bytes rather than JSON.
///
/// Separate from `parse_json` only in what it does on success; the error
/// path is identical, because an image endpoint still answers with a JSON
/// error document when it fails.
pub fn parse_image(resp: &HttpResponse) -> Result<Vec<u8>, ApiError> {
    if !resp.is_success() {
        return Err(ApiError::from_response(resp));
    }
    Ok(resp.body.clone())
}

/// A page of the server's own custom emoji.
///
/// Sorted by name, which the server only honours on this endpoint when
/// asked: the default order is creation time, and a list that changes
/// order as people upload is a list that cannot be paged through
/// without seeing the same emoji twice and missing another.
#[must_use]
pub fn custom_emoji(page: u32, per_page: u32) -> HttpRequest {
    HttpRequest::get(format!(
        "{API}/emoji?page={page}&per_page={per_page}&sort=name"
    ))
}

/// The emoji in one page of that list.
///
/// `null` rather than `[]` on an empty page, for the same reason the
/// drafts endpoint does it -- and with the same consequence if parsed
/// straight into a `Vec`: a server with no custom emoji at all would
/// look like a failed request.
pub fn parse_custom_emoji(resp: &HttpResponse) -> Result<Vec<CustomEmoji>, ApiError> {
    let emoji: Option<Vec<CustomEmoji>> = parse_json(resp, "GET /emoji")?;
    Ok(emoji.unwrap_or_default())
}

/// One custom emoji's image bytes.
///
/// By id rather than by name. The name endpoint exists, but a name is
/// user-supplied text in a URL path, and the id is sixteen hex-ish
/// characters that need no escaping -- and the list this client already
/// holds maps one to the other.
#[must_use]
pub fn custom_emoji_image(emoji: &EmojiId) -> HttpRequest {
    HttpRequest::get(format!("{API}/emoji/{emoji}/image"))
}

/// An attached file's bytes.
///
/// `preview` asks for the server's downscaled copy, which is what a message
/// list wants: the original can be a 12-megapixel phone photo and the row it
/// goes in is four hundred pixels wide. Only ask for it when the file's
/// metadata says the server made one.
#[must_use]
pub fn file_contents(file: &FileId, preview: bool) -> HttpRequest {
    let path = if preview {
        format!("{API}/files/{file}/preview")
    } else {
        format!("{API}/files/{file}")
    };
    HttpRequest::get(path)
}

/// Pin a post to its channel, or take the pin off.
///
/// Two paths rather than a body flag, which is how the server models it.
#[must_use]
pub fn set_pinned(post: &PostId, pinned: bool) -> HttpRequest {
    let verb = if pinned { "pin" } else { "unpin" };
    HttpRequest::post(format!("{API}/posts/{post}/{verb}"))
}

/// Change what a post says.
///
/// `PUT`, not `PATCH`: the patch form takes a sparse body and is the
/// one the webapp uses for props, while this replaces the message and
/// is what "edit" means. The server stamps `edit_at`, and the edited
/// post comes back over the WebSocket as a `post_edited`, which is what
/// actually updates the window -- the same shape as deleting.
///
/// `PostEditTimeLimit` in the client config may forbid it; the server
/// answers 403 and the window says so rather than guessing the rule.
#[must_use]
pub fn edit_post(post: &PostId, message: &str) -> HttpRequest {
    let body = serde_json::json!({ "id": post.to_string(), "message": message });
    HttpRequest::put(format!("{API}/posts/{post}")).json(&body)
}

/// Delete a post. The server soft-deletes; the post comes back over the
/// WebSocket as a `post_deleted`, which is what actually removes it here.
#[must_use]
pub fn delete_post(post: &PostId) -> HttpRequest {
    HttpRequest::delete(format!("{API}/posts/{post}"))
}

/// Mark a post, and everything after it, unread.
///
/// The body is a **native** JSON boolean, unlike most of this API's
/// booleans, which are strings. `collapsed_threads_supported` must match
/// what the client does or the server maintains the wrong counters.
#[must_use]
pub fn set_unread(user: &UserId, post: &PostId, collapsed: bool) -> HttpRequest {
    let body = serde_json::json!({ "collapsed_threads_supported": collapsed });
    HttpRequest::post(format!("{API}/users/{user}/posts/{post}/set_unread")).json(&body)
}

/// Save a post, or take it out of the saved list.
///
/// Saved posts are not an endpoint of their own: they are a *preference*,
/// in the `flagged_post` category, keyed by post id. Saving writes one and
/// unsaving deletes it, which is why this returns a preference request
/// rather than something post-shaped.
///
/// **The two halves do not use the same method.** Writing a preference
/// is `PUT /users/{id}/preferences`; deleting one is
/// `POST /users/{id}/preferences/delete`, because it takes a body and
/// Mattermost models it as an action rather than a `DELETE` on a
/// resource. Sending `PUT` to the delete path answers 404, which reads
/// like a missing post rather than a wrong verb -- and leaves the post
/// saved, with nothing on screen admitting it.
#[must_use]
pub fn set_saved(user: &UserId, post: &PostId, saved: bool) -> HttpRequest {
    let body = serde_json::json!([{
        "user_id": user.as_str(),
        "category": "flagged_post",
        "name": post.as_str(),
        "value": "true",
    }]);
    if saved {
        HttpRequest::put(format!("{API}/users/{user}/preferences")).json(&body)
    } else {
        HttpRequest::post(format!("{API}/users/{user}/preferences/delete")).json(&body)
    }
}

/// Mark a channel read.
///
/// An **empty** `channel` is legal and means focus loss, which is how the web
/// app reports that the window lost focus. `collapsed_threads_supported` must
/// match what the client actually does, because it decides whether the server
/// maintains the root-only counters the client reads.
#[must_use]
pub fn view_channel(
    channel: Option<&ChannelId>,
    previous: Option<&ChannelId>,
    collapsed_threads_supported: bool,
) -> HttpRequest {
    HttpRequest::post(format!("{API}/channels/members/me/view")).json(&serde_json::json!({
        "channel_id": channel.map(ToString::to_string).unwrap_or_default(),
        "prev_channel_id": previous.map(ToString::to_string).unwrap_or_default(),
        "collapsed_threads_supported": collapsed_threads_supported,
    }))
}

/// The response to [`view_channel`].
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ViewResult {
    pub status: String,
    pub last_viewed_at_times: BTreeMap<String, i64>,
}

pub fn parse_view_channel(resp: &HttpResponse) -> Result<ViewResult, ApiError> {
    parse_json(resp, "POST /channels/members/me/view")
}

// ---------------------------------------------------------------- posts

/// How to select a page of posts.
///
/// These are **mutually exclusive on the server** — it checks `since`, then
/// `after`, then `before`, then falls back to paging, and silently ignores the
/// others. Making that an enum means a caller cannot accidentally send two and
/// get a different page than it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostQuery {
    /// Newest page, then older ones by index.
    Page { page: u32, per_page: u32 },
    /// Strictly older than this post.
    Before { post: String, per_page: u32 },
    /// Strictly newer than this post. **This is the right primitive for
    /// closing a gap after a reconnect**: it is properly paginated via
    /// `next_post_id` and it is ETag-cacheable.
    After { post: String, per_page: u32 },
    /// Everything *modified* since a timestamp.
    ///
    /// Use this only to learn about edits and deletes to posts already held.
    /// It has a hard `LIMIT 1000` with no cursor and no overflow signal, it
    /// emits no `ETag`, it returns soft-deleted posts, and its `posts` map is a
    /// superset of `order`. As a backfill primitive it loses data silently.
    Since { millis: i64 },
}

impl PostQuery {
    /// A first page of the default size.
    #[must_use]
    pub fn first() -> Self {
        Self::Page {
            page: 0,
            per_page: DEFAULT_PER_PAGE,
        }
    }

    /// Clamp `per_page` to what the server will actually honour.
    #[must_use]
    pub fn clamped(self) -> Self {
        let clamp = |n: u32| n.clamp(1, MAX_PER_PAGE);
        match self {
            Self::Page { page, per_page } => Self::Page {
                page,
                per_page: clamp(per_page),
            },
            Self::Before { post, per_page } => Self::Before {
                post,
                per_page: clamp(per_page),
            },
            Self::After { post, per_page } => Self::After {
                post,
                per_page: clamp(per_page),
            },
            Self::Since { millis } => Self::Since { millis },
        }
    }
}

#[must_use]
pub fn posts_for_channel(channel: &ChannelId, query: &PostQuery, collapsed: bool) -> HttpRequest {
    let mut req = HttpRequest::get(format!("{API}/channels/{channel}/posts"));
    // Without this the server counts replies against the page size, and a
    // client that hides replies is left with whatever roots happen to be in
    // the sixty posts it was sent. In a channel where most traffic is
    // thread replies that is a handful: one real channel's page of sixty
    // contained five. With it, the page is sixty *roots*.
    if collapsed {
        req = req.q("collapsedThreads", true);
    }
    match query.clone().clamped() {
        PostQuery::Page { page, per_page } => req.q("page", page).q("per_page", per_page),
        PostQuery::Before { post, per_page } => req.q("before", post).q("per_page", per_page),
        PostQuery::After { post, per_page } => req.q("after", post).q("per_page", per_page),
        PostQuery::Since { millis } => req.q("since", millis),
    }
}

pub fn parse_post_list(resp: &HttpResponse) -> Result<PostList, ApiError> {
    parse_json(resp, "GET /channels/{id}/posts")
}

/// One post by id.
///
/// What a permalink resolves to: the link carries a post, and which
/// channel it is in is a thing only the post knows.
#[must_use]
pub fn one_post(post: &PostId) -> HttpRequest {
    HttpRequest::get(format!("{API}/posts/{post}"))
}

#[must_use]
pub fn post_thread(root: &str) -> HttpRequest {
    HttpRequest::get(format!("{API}/posts/{root}/thread"))
}

/// Create a post.
///
/// `pending_post_id` is the client's own id for the message, and it is what
/// matches the optimistic post already on screen to the `posted` event that
/// confirms it. Without it the message visibly duplicates and then jumps.
#[must_use]
pub fn create_post(
    channel: &ChannelId,
    message: &str,
    root: Option<&str>,
    pending_post_id: &str,
    files: &[FileId],
) -> HttpRequest {
    let mut body = serde_json::Map::new();
    body.insert("channel_id".into(), channel.as_str().into());
    body.insert("message".into(), message.into());
    body.insert("pending_post_id".into(), pending_post_id.into());
    // Only when there are some. An empty `file_ids` is accepted, but it
    // makes the request differ from what the web app sends, and those
    // differences are what turn into server-side edge cases later.
    if !files.is_empty() {
        let ids: Vec<serde_json::Value> = files
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect();
        body.insert("file_ids".into(), serde_json::Value::Array(ids));
    }
    // A root post sends no root_id at all. Sending `""` is accepted but makes
    // the request differ from what the web app sends, which is the kind of
    // difference that turns into a server-side edge case later.
    if let Some(root) = root.filter(|r| !r.is_empty()) {
        body.insert("root_id".into(), root.into());
    }
    HttpRequest::post(format!("{API}/posts")).json(&body.into())
}

pub fn parse_post(resp: &HttpResponse) -> Result<Post, ApiError> {
    parse_json(resp, "POST /posts")
}

// ---------------------------------------------------------------- users

/// Look up users by id, for filling in post authors and DM names.
#[must_use]
pub fn users_by_ids(ids: &[String]) -> HttpRequest {
    let body: serde_json::Value = ids.iter().map(String::as_str).collect();
    HttpRequest::post(format!("{API}/users/ids")).json(&body)
}

pub fn parse_users(resp: &HttpResponse) -> Result<Vec<User>, ApiError> {
    parse_json(resp, "POST /users/ids")
}

/// Everyone in a channel, which is who `@` may mean while reading it.
///
/// Fetched when the channel is opened rather than per keystroke: the
/// answer is a page of users and it is the same answer for every letter
/// typed, so asking once is both faster and quieter than the
/// autocomplete endpoint Mattermost's web app calls.
#[must_use]
pub fn users_in_channel(channel: &ChannelId, per_page: u32) -> HttpRequest {
    HttpRequest::get(format!("{API}/users"))
        .q("in_channel", channel)
        .q("per_page", per_page)
}

pub fn parse_users_in_channel(resp: &HttpResponse) -> Result<Vec<User>, ApiError> {
    parse_json(resp, "GET /users?in_channel")
}

/// Presence for a set of people, in one request.
///
/// `POST` rather than `GET` because the ids go in the body: a channel
/// roster is two hundred ids, and a query string of those is both over
/// the usual proxy limit and a log line naming everybody the reader is
/// looking at.
#[must_use]
pub fn statuses_for(users: &[UserId]) -> HttpRequest {
    let ids: Vec<serde_json::Value> = users
        .iter()
        .map(|id| serde_json::Value::String(id.to_string()))
        .collect();
    HttpRequest::post(format!("{API}/users/status/ids")).json(&serde_json::Value::Array(ids))
}

pub fn parse_statuses(resp: &HttpResponse) -> Result<Vec<Status>, ApiError> {
    parse_json(resp, "POST /users/status/ids")
}

/// Say where this reader is.
///
/// `PUT`, and the body repeats the user id the path already carries --
/// which is what the server wants, not a redundancy worth trimming:
/// it rejects a body without it. Setting a status this way marks it
/// **manual**, so the server stops moving it on its own, which is the
/// whole point of choosing one.
#[must_use]
pub fn set_status(user: &UserId, presence: mm_types::Presence) -> HttpRequest {
    HttpRequest::put(format!("{API}/users/{user}/status")).json(&serde_json::json!({
        "user_id": user.as_str(),
        "status": presence.as_str(),
    }))
}

pub fn parse_status(resp: &HttpResponse) -> Result<Status, ApiError> {
    parse_json(resp, "PUT /users/{user}/status")
}

// ---------------------------------------------------------------- prefs

#[must_use]
pub fn my_preferences() -> HttpRequest {
    HttpRequest::get(format!("{API}/users/me/preferences"))
}

pub fn parse_preferences(resp: &HttpResponse) -> Result<Vec<Preference>, ApiError> {
    parse_json(resp, "GET /users/me/preferences")
}

#[cfg(test)]
#[path = "tests/rest.rs"]
mod tests;
