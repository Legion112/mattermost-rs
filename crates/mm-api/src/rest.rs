//! Request builders and response parsers for the REST API.
//!
//! Every function here is pure. Builders return an [`HttpRequest`]; parsers
//! take an [`HttpResponse`] and return a model or an [`ApiError`].

use crate::error::ApiError;
use crate::http::{HttpRequest, HttpResponse};
use mm_types::{
    CategoryList, Channel, ChannelId, ChannelMember, ChannelStats, Post, PostList, Preference,
    Team, TeamId, User, UserId,
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

/// How many people are in a channel, for the channel header.
#[must_use]
pub fn channel_stats(channel: &ChannelId) -> HttpRequest {
    HttpRequest::get(format!("{API}/channels/{channel}/stats"))
}

pub fn parse_channel_stats(resp: &HttpResponse) -> Result<ChannelStats, ApiError> {
    parse_json(resp, "GET /channels/{id}/stats")
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
pub fn posts_for_channel(channel: &ChannelId, query: &PostQuery) -> HttpRequest {
    let req = HttpRequest::get(format!("{API}/channels/{channel}/posts"));
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
) -> HttpRequest {
    let mut body = serde_json::Map::new();
    body.insert("channel_id".into(), channel.as_str().into());
    body.insert("message".into(), message.into());
    body.insert("pending_post_id".into(), pending_post_id.into());
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
