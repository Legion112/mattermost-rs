//! Fill a fresh lab server with something worth looking at.
//!
//! The fixture is chosen to exercise this client rather than to look like a
//! real company: a thread, a mention, a reaction, a code block, an
//! attachment, a custom emoji, a private channel, a DM, and one public
//! channel that exists only so that joining and leaving it costs nothing.
//!
//! Seeding is over the REST API rather than `mmctl --local`, for one reason:
//! every call here goes through the same `mm_api` builders the client uses,
//! so a route that the server has changed breaks the seed as loudly as it
//! breaks the client, in the same place.

use mm_api::http::{HttpRequest, Method};
use serde_json::{Value, json};

use crate::{Error, Result};

/// One password for every seeded account. The server's default minimum is
/// eight characters, which is why this is not "lab".
pub const PASSWORD: &str = "labpassword";

/// The first account created, which the server promotes to system admin.
pub const ADMIN: &str = "legion";

/// The team's URL name; its display name is "Lab".
pub const TEAM: &str = "lab";

/// A seeded colleague.
struct Person {
    username: &'static str,
    first: &'static str,
    last: &'static str,
    position: &'static str,
}

const PEOPLE: &[Person] = &[
    Person {
        username: "ada",
        first: "Ada",
        last: "Lovelace",
        position: "Analyst",
    },
    Person {
        username: "grace",
        first: "Grace",
        last: "Hopper",
        position: "Compilers",
    },
    Person {
        username: "linus",
        first: "Linus",
        last: "Pauling",
        position: "Chemistry",
    },
    Person {
        username: "margaret",
        first: "Margaret",
        last: "Hamilton",
        position: "Flight software",
    },
    Person {
        username: "oscar",
        first: "Oscar",
        last: "Wilde",
        position: "Prose",
    },
];

/// `(url name, display name, private?, who is in it besides the admin)`
const CHANNELS: &[(&str, &str, bool, &[&str])] = &[
    (
        "engineering",
        "Engineering",
        false,
        &["ada", "grace", "margaret"],
    ),
    ("design", "Design", false, &["oscar", "ada"]),
    (
        "random-lab",
        "Random",
        false,
        &["ada", "grace", "linus", "margaret", "oscar"],
    ),
    // Exists to be joined and left. Leaving a channel posts a system message
    // into it, so testing that on a real server is a thing colleagues see;
    // here it is the channel's entire purpose.
    (
        "leaving-practice",
        "Leaving Practice",
        false,
        &["grace", "linus"],
    ),
    ("secret-plans", "Secret Plans", true, &["margaret"]),
    // Exists to be archived and brought back, for the same reason.
    ("archive-practice", "Archive Practice", false, &["ada"]),
];

/// A 16x16 PNG of a flat square, for the custom emoji. Generated rather than
/// read from `assets/` so that seeding needs no file outside the binary.
fn swatch_png() -> Vec<u8> {
    // Written out by hand rather than pulled in with an image crate: this is
    // the one image the lab needs and it never changes.
    const BYTES: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x91, 0x68, 0x36, 0x00, 0x00, 0x00, 0x1f, 0x49, 0x44, 0x41, 0x54, 0x38, 0x8d, 0x63, 0xfc,
        0xcf, 0x80, 0x1b, 0x18, 0x18, 0xfe, 0xff, 0xff, 0x3f, 0x03, 0x29, 0x80, 0x89, 0x81, 0x44,
        0x30, 0x31, 0x90, 0x08, 0x26, 0x06, 0x12, 0xc1, 0x0c, 0x00, 0x00, 0x8c, 0x30, 0x09, 0xd3,
        0x1b, 0x95, 0x33, 0xa6, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
        0x82,
    ];
    BYTES.to_vec()
}

/// A client bound to the lab, optionally carrying somebody's token.
#[derive(Debug)]
/// Behind a lock so that a session the lab revokes on purpose can be
/// replaced without every `Admin` method taking `&mut self`.
struct Api(tokio::sync::RwLock<mm_http::Client>);

impl Api {
    fn anonymous() -> Result<Self> {
        mm_http::Client::new(crate::stack::URL)
            .map(|client| Self(tokio::sync::RwLock::new(client)))
            .map_err(|e| Error(e.to_string()))
    }

    fn with_token(token: &str) -> Result<mm_http::Client> {
        Ok(mm_http::Client::new(crate::stack::URL)
            .map_err(|e| Error(e.to_string()))?
            .with_token(token))
    }

    fn as_token(token: &str) -> Result<Self> {
        Ok(Self(tokio::sync::RwLock::new(Self::with_token(token)?)))
    }

    /// Sign in again and carry on with the new token.
    async fn relogin(&self, username: &str) -> Result<()> {
        let (token, _) = Self::anonymous()?.login(username).await?;
        *self.0.write().await = Self::with_token(&token)?;
        Ok(())
    }

    /// Send, and fail with the server's own message rather than a status code.
    async fn call(&self, req: &HttpRequest) -> Result<Value> {
        let resp = self
            .0
            .read()
            .await
            .send(req)
            .await
            .map_err(|e| Error(format!("{} {}: {e}", req.method, req.path)))?;
        if !resp.is_success() {
            return Err(Error(format!(
                "{} {} -> {}: {}",
                req.method,
                req.path,
                resp.status,
                String::from_utf8_lossy(&resp.body).trim()
            )));
        }
        if resp.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&resp.body).map_err(|e| {
            Error(format!(
                "{} {}: body did not parse: {e}",
                req.method, req.path
            ))
        })
    }

    /// Send, and return `None` when the server says it already exists.
    ///
    /// Seeding twice should be a no-op rather than an error, because the
    /// usual reason to run it again is that the first run died halfway.
    async fn create(&self, req: &HttpRequest) -> Result<Option<Value>> {
        match self.call(req).await {
            Ok(v) => Ok(Some(v)),
            Err(Error(msg)) if msg.contains("-> 400") || msg.contains("-> 403") => {
                if msg.contains("exist") || msg.contains("taken") || msg.contains("unique") {
                    Ok(None)
                } else {
                    Err(Error(msg))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// How many posts somebody actually wrote in a channel.
    ///
    /// A system notice -- "x added to the channel by y" -- carries a `type`
    /// beginning `system_`, and every other post carries an empty one. The
    /// distinction is what makes "has this been seeded" answerable.
    async fn written_posts(&self, channel: &str) -> Result<usize> {
        let page = self
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{channel}/posts"
            )))
            .await?;
        let Some(posts) = page["posts"].as_object() else {
            return Ok(0);
        };
        Ok(posts
            .values()
            .filter(|p| p["type"].as_str().unwrap_or_default().is_empty())
            .count())
    }

    async fn login(&self, username: &str) -> Result<(String, String)> {
        let req = mm_api::rest::login(&mm_api::rest::Login {
            login_id: username.to_owned(),
            password: PASSWORD.to_owned(),
            mfa_token: None,
        });
        let resp = self
            .0
            .read()
            .await
            .send(&req)
            .await
            .map_err(|e| Error(format!("login {username}: {e}")))?;
        if !resp.is_success() {
            return Err(Error(format!(
                "login {username} -> {}: {}",
                resp.status,
                String::from_utf8_lossy(&resp.body).trim()
            )));
        }
        let session = mm_api::rest::parse_login(&resp)
            .map_err(|e| Error(format!("login {username}: {e}")))?;
        Ok((session.token, session.user.id.to_string()))
    }
}

fn id_of(value: &Value) -> Result<String> {
    value["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error(format!("expected an object with an id, got {value}")))
}

/// Create everything, and say what was created.
///
/// Safe to run twice: anything already present is left alone.
pub async fn seed() -> Result<()> {
    seed_with(0, 0).await
}

/// Seed, plus `bulk` further channels with one message each.
///
/// For measuring anything that scales with how many channels an account
/// is in -- background warming, above all. A lab of five channels says
/// nothing about a work account in sixty, and sixty is the shape the
/// complaint came from.
pub async fn seed_with(bulk: usize, long: usize) -> Result<()> {
    let anon = Api::anonymous()?;

    // The first non-bot account on a server is promoted to system admin, and
    // the "is the server open" check is skipped for it. That only works while
    // there are no users, so it is the first thing done and the one call
    // whose failure might just mean "already seeded".
    anon.create(&signup(ADMIN, "Legion", "Admin", "Owner"))
        .await?;
    let (admin_token, admin_id) = anon.login(ADMIN).await?;
    let admin = Api::as_token(&admin_token)?;
    println!("admin {ADMIN} / {PASSWORD}");

    let team = find_or_make_team(&admin).await?;

    // Everyone else, created by the admin, then put on the team.
    let mut ids = std::collections::BTreeMap::new();
    let mut tokens = std::collections::BTreeMap::new();
    for person in PEOPLE {
        admin
            .create(&signup(
                person.username,
                person.first,
                person.last,
                person.position,
            ))
            .await?;
        let (token, id) = anon.login(person.username).await?;
        admin
            .create(
                &HttpRequest::new(Method::Post, format!("/api/v4/teams/{team}/members"))
                    .json(&json!({ "team_id": team, "user_id": id })),
            )
            .await?;
        ids.insert(person.username, id);
        tokens.insert(person.username, token);
    }
    println!("{} colleagues on team {TEAM}", PEOPLE.len());

    let emoji = add_custom_emoji(&admin, &admin_id).await?;
    give_faces(&admin, &ids).await?;

    let mut channels = std::collections::BTreeMap::new();
    for (name, display, private, members) in CHANNELS {
        let channel = find_or_make_channel(&admin, &team, name, display, *private).await?;
        for member in *members {
            let Some(id) = ids.get(member) else { continue };
            admin
                .create(
                    &HttpRequest::new(Method::Post, format!("/api/v4/channels/{channel}/members"))
                        .json(&json!({ "user_id": id })),
                )
                .await?;
        }
        channels.insert(*name, channel);
    }
    println!("{} channels", channels.len());

    converse(&admin, &tokens, &channels, &admin_id, &emoji).await?;
    direct_message(&admin, &tokens, &ids, &admin_id).await?;
    if bulk > 0 {
        bulk_channels(&admin, &team, bulk).await?;
    }
    if long > 0 {
        long_channel(&admin, &team, &tokens, long).await?;
    }

    println!(
        "\nseeded. Point the client at {}\n  server    {}\n  username  {ADMIN}\n  password  {PASSWORD}",
        crate::stack::URL,
        crate::stack::URL
    );
    Ok(())
}

fn signup(username: &str, first: &str, last: &str, position: &str) -> HttpRequest {
    HttpRequest::new(Method::Post, "/api/v4/users").json(&json!({
        "email": format!("{username}@mmrs.lab"),
        "username": username,
        "password": PASSWORD,
        "first_name": first,
        "last_name": last,
        "position": position,
    }))
}

async fn find_or_make_team(admin: &Api) -> Result<String> {
    let made = admin
        .create(
            &HttpRequest::new(Method::Post, "/api/v4/teams").json(&json!({
                "name": TEAM,
                "display_name": "Lab",
                "type": "O",
            })),
        )
        .await?;
    if let Some(team) = made {
        return id_of(&team);
    }
    let found = admin
        .call(&HttpRequest::get(format!("/api/v4/teams/name/{TEAM}")))
        .await?;
    id_of(&found)
}

async fn find_or_make_channel(
    admin: &Api,
    team: &str,
    name: &str,
    display: &str,
    private: bool,
) -> Result<String> {
    let made = admin
        .create(
            &HttpRequest::new(Method::Post, "/api/v4/channels").json(&json!({
                "team_id": team,
                "name": name,
                "display_name": display,
                "type": if private { "P" } else { "O" },
            })),
        )
        .await?;
    if let Some(channel) = made {
        return id_of(&channel);
    }
    // Among the archived too: a run that archived a channel and died
    // before restoring it must not leave the next seed unable to find
    // it, or to make it, since the name is still taken.
    let found = admin
        .call(&HttpRequest::get(format!(
            "/api/v4/teams/{team}/channels/name/{name}?include_deleted=true"
        )))
        .await?;
    let id = id_of(&found)?;
    if found["delete_at"].as_i64().unwrap_or(0) > 0 {
        admin
            .call(&HttpRequest::new(
                Method::Post,
                format!("/api/v4/channels/{id}/restore"),
            ))
            .await?;
        println!("restored archived channel {name}");
    }
    Ok(id)
}

/// Upload `:lab:` so the inline custom-emoji path has something to render.
async fn add_custom_emoji(admin: &Api, admin_id: &str) -> Result<String> {
    let req = HttpRequest::new(Method::Post, "/api/v4/emoji").multipart(
        vec![(
            "emoji".to_owned(),
            json!({ "creator_id": admin_id, "name": "lab" }).to_string(),
        )],
        "image",
        "lab.png",
        swatch_png(),
    );
    match admin.create(&req).await? {
        Some(_) => println!("custom emoji :lab:"),
        None => println!("custom emoji :lab: already there"),
    }
    Ok("lab".to_owned())
}

/// Post as the seeded people, so the channels have history with more than one
/// author in it.
async fn converse(
    admin: &Api,
    tokens: &std::collections::BTreeMap<&str, String>,
    channels: &std::collections::BTreeMap<&str, String>,
    admin_id: &str,
    emoji: &str,
) -> Result<()> {
    let Some(engineering) = channels.get("engineering") else {
        return Ok(());
    };

    // Already seeded? Posting again would double every message, and a
    // channel with the fixture in it twice is worse than one with it once.
    //
    // Counting *written* posts, not posts: adding somebody to a channel
    // puts a system join notice in it, so a brand new channel already has
    // one per member and a plain count says "seeded" on the first run.
    let written = admin.written_posts(engineering).await?;
    if written > 0 {
        println!("conversation already seeded ({written} messages)");
        return Ok(());
    }

    let ada = Api::as_token(tokens.get("ada").map_or("", String::as_str))?;
    let grace = Api::as_token(tokens.get("grace").map_or("", String::as_str))?;
    let margaret = Api::as_token(tokens.get("margaret").map_or("", String::as_str))?;

    let root = id_of(
        &ada.call(&post(
            engineering,
            "Morning. The nightly run went red again — `mm-core` store tests, \
             not the transport this time.",
            None,
        ))
        .await?,
    )?;
    grace
        .call(&post(
            engineering,
            "Looking. Is it the ordering one?\n\n```rust\nassert_eq!(store.channels(), \
             expected);\n```",
            Some(&root),
        ))
        .await?;
    margaret
        .call(&post(
            engineering,
            &format!("@{ADMIN} this one is yours I think :{emoji}:"),
            Some(&root),
        ))
        .await?;
    ada.call(&post(
        engineering,
        "Three things before standup:\n\n- the red test\n- the release tag\n- \
         [the protocol notes](https://docs.mattermost.com/)\n\nNothing else from me.",
        None,
    ))
    .await?;

    // A reaction on the thread root, so the reaction row has something in it.
    admin
        .create(
            &HttpRequest::new(Method::Post, "/api/v4/reactions").json(&json!({
                "user_id": admin_id,
                "post_id": root,
                "emoji_name": "eyes",
            })),
        )
        .await?;

    // An attachment, which is a different render path from everything above.
    if let Some(random) = channels.get("random-lab") {
        let upload = admin
            .call(&mm_api::rest::upload_file(
                &mm_types::ChannelId::from(random.clone()),
                "swatch.png",
                swatch_png(),
            ))
            .await?;
        if let Some(file) = upload["file_infos"][0]["id"].as_str() {
            admin
                .call(
                    &HttpRequest::new(Method::Post, "/api/v4/posts").json(&json!({
                        "channel_id": random,
                        "message": "One pixel of colour for the afternoon.",
                        "file_ids": [file],
                    })),
                )
                .await?;
        }
    }

    println!("seeded a conversation, a thread, a reaction and an attachment");
    Ok(())
}

async fn direct_message(
    admin: &Api,
    tokens: &std::collections::BTreeMap<&str, String>,
    ids: &std::collections::BTreeMap<&str, String>,
    admin_id: &str,
) -> Result<()> {
    let Some(ada_id) = ids.get("ada") else {
        return Ok(());
    };
    let channel = id_of(
        &admin
            .call(
                &HttpRequest::new(Method::Post, "/api/v4/channels/direct")
                    .json(&json!([admin_id, ada_id])),
            )
            .await?,
    )?;
    if admin.written_posts(&channel).await? > 0 {
        return Ok(());
    }
    let ada = Api::as_token(tokens.get("ada").map_or("", String::as_str))?;
    ada.call(&post(&channel, "Are you about this afternoon?", None))
        .await?;
    println!("seeded a direct message");
    Ok(())
}

fn post(channel: &str, message: &str, root: Option<&str>) -> HttpRequest {
    let mut body = json!({ "channel_id": channel, "message": message });
    if let Some(root) = root {
        body["root_id"] = json!(root);
    }
    HttpRequest::new(Method::Post, "/api/v4/posts").json(&body)
}

/// Set a seeded account's status, as that account: what makes the
/// server send everybody a `status_change`.
///
/// For measuring what a status change costs the window, which on a
/// real account is a steady drip of them all day.
pub async fn set_status(username: &str, status: &str) -> Result<()> {
    let them = Api::as_token(&token_for(username).await?)?;
    let me = id_of(&them.call(&HttpRequest::get("/api/v4/users/me")).await?)?;
    them.call(
        &HttpRequest::new(Method::Put, format!("/api/v4/users/{me}/status"))
            .json(&json!({ "user_id": me, "status": status })),
    )
    .await
    .map(|_| ())
}

/// Set a seeded account's custom status, as that account: what makes
/// the server send everybody a `user_updated` carrying the prop.
pub async fn set_custom_status(username: &str, emoji: &str, text: &str) -> Result<()> {
    let them = Api::as_token(&token_for(username).await?)?;
    them.call(
        &HttpRequest::new(Method::Put, "/api/v4/users/me/status/custom")
            .json(&json!({ "emoji": emoji, "text": text })),
    )
    .await
    .map(|_| ())
}

/// Clear it again, as that account.
pub async fn clear_custom_status(username: &str) -> Result<()> {
    let them = Api::as_token(&token_for(username).await?)?;
    them.call(&HttpRequest::new(
        Method::Delete,
        "/api/v4/users/me/status/custom",
    ))
    .await
    .map(|_| ())
}

/// Log in as a seeded account and hand back the token.
///
/// For the lab's own tools, which need to be somebody to watch a socket.
pub async fn token_for(username: &str) -> Result<String> {
    Api::anonymous()?
        .login(username)
        .await
        .map(|(token, _)| token)
}

/// What the *server* says, for a test to check the window against.
///
/// A UI test that only reads the window proves the window is
/// self-consistent, which is not the same as right: the whole class of bug
/// where a button updates the screen and never reaches the server would
/// pass. These are the second opinion.
#[derive(Debug)]
pub struct Admin {
    api: Api,
    team: String,
    me: String,
}

impl Admin {
    pub async fn connect() -> Result<Self> {
        let anon = Api::anonymous()?;
        let (token, me) = anon.login(ADMIN).await?;
        let api = Api::as_token(&token)?;
        let team = id_of(
            &api.call(&HttpRequest::get(format!("/api/v4/teams/name/{TEAM}")))
                .await?,
        )?;
        Ok(Self { api, team, me })
    }

    async fn channel(&self, name: &str) -> Result<String> {
        // Among the archived too: a test that archives a channel still
        // has to find it, to bring it back.
        let found = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/teams/{}/channels/name/{name}?include_deleted=true",
                self.team
            )))
            .await?;
        id_of(&found)
    }

    /// End every session the signed-in user has, the lab's own included,
    /// and sign the lab back in. The client under test is the same user,
    /// so this is what a revoked session looks like from its side.
    pub async fn revoke_my_sessions(&self) -> Result<()> {
        self.api
            .call(&HttpRequest::new(
                Method::Post,
                format!("/api/v4/users/{}/sessions/revoke/all", self.me),
            ))
            .await?;
        self.api.relogin(ADMIN).await
    }

    /// How many people the server has in a channel.
    pub async fn members(&self, channel: &str) -> Result<usize> {
        let id = self.channel(channel).await?;
        let list = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/members?per_page=200"
            )))
            .await?;
        Ok(list.as_array().map_or(0, Vec::len))
    }

    /// The admin's own custom status text, as the server holds it:
    /// `None` once it has been cleared, which the server records as a
    /// prop with empty fields rather than no prop.
    pub async fn my_custom_status(&self) -> Result<Option<String>> {
        let me = self
            .api
            .call(&HttpRequest::get(format!("/api/v4/users/{}", self.me)))
            .await?;
        let Some(raw) = me.pointer("/props/customStatus").and_then(Value::as_str) else {
            return Ok(None);
        };
        let status: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        Ok(status
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned))
    }

    /// Save a root post, or unsave it, as the admin -- through the
    /// preference the webapp writes, which is what another client of
    /// this account would do, and what the window hears over the socket.
    pub async fn save_post(
        &self,
        channel: &str,
        root_starts_with: &str,
        saved: bool,
    ) -> Result<()> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=200"
            )))
            .await?;
        let root_id = id_of(&find_root(&page, root_starts_with)?)?;
        let pref = json!([{
            "user_id": self.me,
            "category": "flagged_post",
            "name": root_id,
            "value": "true",
        }]);
        let request = if saved {
            HttpRequest::new(
                Method::Put,
                format!("/api/v4/users/{}/preferences", self.me),
            )
            .json(&pref)
        } else {
            HttpRequest::new(
                Method::Post,
                format!("/api/v4/users/{}/preferences/delete", self.me),
            )
            .json(&pref)
        };
        self.api.call(&request).await.map(|_| ())
    }

    /// A seeded colleague's id.
    async fn user_id(&self, username: &str) -> Result<String> {
        id_of(
            &self
                .api
                .call(&HttpRequest::get(format!(
                    "/api/v4/users/username/{username}"
                )))
                .await?,
        )
    }

    /// Post in a channel as a colleague, so the window hears it over the
    /// socket. Hands back the post's id.
    pub async fn say_as(&self, username: &str, channel: &str, text: &str) -> Result<String> {
        let id = self.channel(channel).await?;
        let them = Api::as_token(&token_for(username).await?)?;
        id_of(&them.call(&post(&id, text, None)).await?)
    }

    /// React to a post as a colleague.
    pub async fn react_as(&self, username: &str, post_id: &str, emoji: &str) -> Result<()> {
        let them = Api::as_token(&token_for(username).await?)?;
        let me = self.user_id(username).await?;
        them.call(
            &HttpRequest::new(Method::Post, "/api/v4/reactions").json(&json!({
                "user_id": me,
                "post_id": post_id,
                "emoji_name": emoji,
            })),
        )
        .await
        .map(|_| ())
    }

    /// Change what a colleague's post says, as them.
    pub async fn edit_as(&self, username: &str, post_id: &str, text: &str) -> Result<()> {
        let them = Api::as_token(&token_for(username).await?)?;
        them.call(
            &HttpRequest::new(Method::Put, format!("/api/v4/posts/{post_id}/patch"))
                .json(&json!({ "message": text })),
        )
        .await
        .map(|_| ())
    }

    /// Delete a colleague's post, as them.
    pub async fn delete_as(&self, username: &str, post_id: &str) -> Result<()> {
        let them = Api::as_token(&token_for(username).await?)?;
        them.call(&HttpRequest::new(
            Method::Delete,
            format!("/api/v4/posts/{post_id}"),
        ))
        .await
        .map(|_| ())
    }

    /// Say, as a colleague, that they are typing in a channel: the
    /// server turns it into the `typing` frame everybody else gets.
    pub async fn typing_as(&self, username: &str, channel: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        let me = self.user_id(username).await?;
        let them = Api::as_token(&token_for(username).await?)?;
        them.call(
            &HttpRequest::new(Method::Post, format!("/api/v4/users/{me}/typing"))
                .json(&json!({ "channel_id": id, "parent_id": "" })),
        )
        .await
        .map(|_| ())
    }

    /// Open a direct message to the admin from a colleague who has none
    /// yet, and say something in it.
    pub async fn dm_from(&self, username: &str, text: &str) -> Result<()> {
        let who = self.user_id(username).await?;
        let them = Api::as_token(&token_for(username).await?)?;
        let channel = id_of(
            &them
                .call(
                    &HttpRequest::new(Method::Post, "/api/v4/channels/direct")
                        .json(&json!([who, self.me])),
                )
                .await?,
        )?;
        them.call(&post(&channel, text, None)).await.map(|_| ())
    }

    /// The admin's own membership of a channel, notify props and all.
    pub async fn my_membership(&self, channel: &str) -> Result<Value> {
        let id = self.channel(channel).await?;
        self.api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/members/{}",
                self.me
            )))
            .await
    }

    /// How many messages the server counts unread for the admin in a
    /// channel.
    pub async fn my_unread(&self, channel: &str) -> Result<i64> {
        let id = self.channel(channel).await?;
        let unread = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/{}/channels/{id}/unread",
                self.me
            )))
            .await?;
        Ok(unread["msg_count"].as_i64().unwrap_or(-1))
    }

    /// The texts of the admin's drafts on the team, as the server holds
    /// them.
    pub async fn my_drafts(&self) -> Result<Vec<String>> {
        let drafts = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/{}/teams/{}/drafts",
                self.me, self.team
            )))
            .await?;
        Ok(drafts
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|d| d["message"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Whether a channel holds a message saying exactly this, as the
    /// server has it.
    pub async fn channel_says(&self, channel: &str, text: &str) -> Result<bool> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=60"
            )))
            .await?;
        Ok(page["posts"]
            .as_object()
            .is_some_and(|posts| posts.values().any(|p| p["message"].as_str() == Some(text))))
    }

    /// The newest message in a channel, and the names of the reactions
    /// on it, as the server has them.
    pub async fn newest_with_reactions(&self, channel: &str) -> Result<(String, Vec<String>)> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=1"
            )))
            .await?;
        let newest = page["order"][0]
            .as_str()
            .ok_or_else(|| Error(format!("{channel} has no messages")))?
            .to_owned();
        let post = &page["posts"][&newest];
        let text = post["message"].as_str().unwrap_or_default().to_owned();
        let reactions = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/posts/{newest}/reactions"
            )))
            .await?;
        let names = reactions
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|r| r["emoji_name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok((text, names))
    }

    /// The admin's presence as the server holds it, and when a timed
    /// do-not-disturb ends (seconds since the epoch; zero for never).
    pub async fn my_status(&self) -> Result<(String, i64)> {
        let status = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/{}/status",
                self.me
            )))
            .await?;
        Ok((
            status["status"].as_str().unwrap_or_default().to_owned(),
            status["dnd_end_time"].as_i64().unwrap_or(0),
        ))
    }

    /// The id of the root post in a channel whose text starts a given way.
    pub async fn post_id_of(&self, channel: &str, starts_with: &str) -> Result<String> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=200"
            )))
            .await?;
        id_of(&find_root(&page, starts_with)?)
    }

    /// One of the signed-in user's preferences, as the server holds it.
    pub async fn my_preference(&self, category: &str, name: &str) -> Result<Option<String>> {
        let all = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/{}/preferences/{category}",
                self.me
            )))
            .await?;
        Ok(all.as_array().and_then(|prefs| {
            prefs
                .iter()
                .find(|p| p["name"].as_str() == Some(name))
                .and_then(|p| p["value"].as_str())
                .map(str::to_owned)
        }))
    }

    /// Whether the server holds a post pinned.
    pub async fn is_pinned(&self, post_id: &str) -> Result<bool> {
        let post = self
            .api
            .call(&HttpRequest::get(format!("/api/v4/posts/{post_id}")))
            .await?;
        Ok(post["is_pinned"].as_bool().unwrap_or(false))
    }

    /// How many replies the server holds under a root.
    pub async fn replies_to(&self, channel: &str, starts_with: &str) -> Result<usize> {
        let root = self.post_id_of(channel, starts_with).await?;
        let thread = self
            .api
            .call(&HttpRequest::get(format!("/api/v4/posts/{root}/thread")))
            .await?;
        Ok(thread["posts"].as_object().map_or(0, |posts| {
            posts
                .values()
                .filter(|p| p["root_id"].as_str() == Some(root.as_str()))
                .count()
        }))
    }

    /// Put a small file on the server for a channel, as the admin, the
    /// way the webapp does before it writes a draft naming it.
    pub async fn upload_note(&self, channel: &str, name: &str, bytes: &[u8]) -> Result<String> {
        let id = self.channel(channel).await?;
        let answer = self
            .api
            .call(&HttpRequest::new(Method::Post, "/api/v4/files").multipart(
                vec![("channel_id".to_owned(), id)],
                "files",
                name,
                bytes.to_vec(),
            ))
            .await?;
        answer["file_infos"][0]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error(format!("the server named no file for {name}: {answer}")))
    }

    /// Write a draft naming a file, as another client of this account
    /// would.
    pub async fn draft_with_file(&self, channel: &str, text: &str, file: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        self.api
            .call(
                &HttpRequest::new(Method::Post, "/api/v4/drafts").json(&json!({
                    "channel_id": id,
                    "message": text,
                    "file_ids": [file],
                })),
            )
            .await
            .map(|_| ())
    }

    /// The files on the message in a channel saying exactly this.
    pub async fn files_on(&self, channel: &str, text: &str) -> Result<Vec<String>> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=60"
            )))
            .await?;
        Ok(page["posts"]
            .as_object()
            .and_then(|posts| {
                posts
                    .values()
                    .find(|p| p["message"].as_str().map(str::trim) == Some(text.trim()))
            })
            .and_then(|p| p["file_ids"].as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A channel's record as the server holds it: name, header, purpose,
    /// and whether it is archived.
    pub async fn channel_record(&self, channel: &str) -> Result<Value> {
        let id = self.channel(channel).await?;
        self.api
            .call(&HttpRequest::get(format!("/api/v4/channels/{id}")))
            .await
    }

    /// The ids of the admin's saved posts, as the server holds them.
    pub async fn my_saved_ids(&self) -> Result<Vec<String>> {
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/{}/posts/flagged?per_page=60",
                self.me
            )))
            .await?;
        Ok(page["posts"]
            .as_object()
            .map(|posts| posts.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// The admin's own record, as the server holds it.
    pub async fn my_record(&self) -> Result<Value> {
        self.api
            .call(&HttpRequest::get(format!("/api/v4/users/{}", self.me)))
            .await
    }

    /// Make a channel public or private, as the admin: how a test puts a
    /// channel back the way the seed made it.
    pub async fn set_privacy(&self, channel: &str, private: bool) -> Result<()> {
        let id = self.channel(channel).await?;
        self.api
            .call(
                &HttpRequest::new(Method::Put, format!("/api/v4/channels/{id}/privacy"))
                    .json(&json!({ "privacy": if private { "P" } else { "O" } })),
            )
            .await
            .map(|_| ())
    }

    /// Whether the admin has a direct-message channel with somebody.
    pub async fn has_dm_with(&self, username: &str) -> Result<bool> {
        let who = self.user_id(username).await?;
        let mine = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/me/teams/{}/channels",
                self.team
            )))
            .await?;
        Ok(mine.as_array().is_some_and(|list| {
            list.iter().any(|c| {
                c["type"] == "D" && c["name"].as_str().is_some_and(|name| name.contains(&who))
            })
        }))
    }

    /// Whether the signed-in account is in a channel.
    pub async fn am_in(&self, channel: &str) -> Result<bool> {
        let mine = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/users/me/teams/{}/channels",
                self.team
            )))
            .await?;
        Ok(mine
            .as_array()
            .is_some_and(|list| list.iter().any(|c| c["name"] == *channel)))
    }

    /// Reply, as somebody else, to the thread in `channel` whose root
    /// starts with `root_starts_with`. Says how many replies the root
    /// had before this one.
    ///
    /// For a scenario about a thread moving while the window is open:
    /// the reply has to come from another account, over the socket,
    /// while the client is looking the other way.
    pub async fn reply_as(
        &self,
        username: &str,
        channel: &str,
        root_starts_with: &str,
        text: &str,
    ) -> Result<i64> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=200"
            )))
            .await?;
        let root = find_root(&page, root_starts_with)?;
        // Counted, not read: the page carries `reply_count` only when
        // asked for with collapsed threads, and this is not.
        let root_id = id_of(&root)?;
        let before = page["posts"].as_object().map_or(0, |posts| {
            posts
                .values()
                .filter(|p| p["root_id"].as_str() == Some(root_id.as_str()))
                .count()
        });
        let them = Api::as_token(&token_for(username).await?)?;
        them.call(&post(&id, text, Some(&root_id))).await?;
        i64::try_from(before).map_err(|e| Error(e.to_string()))
    }

    /// Delete what the scenarios leave in a channel -- the add/remove
    /// notices, and every message the lab sent, edited or replied with
    /// -- so a lab kept across runs stays the shape the seed made.
    /// Forty leftovers push the seeded thread above the rows the window
    /// builds, and every test that looks for it then finds nothing.
    pub async fn sweep_traces(&self, channel: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        let page = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/channels/{id}/posts?per_page=200"
            )))
            .await?;
        let notices: Vec<String> = page["posts"]
            .as_object()
            .map(|posts| {
                posts
                    .values()
                    .filter(|p| {
                        matches!(
                            p["type"].as_str(),
                            Some(
                                "system_add_to_channel"
                                    | "system_remove_from_channel"
                                    | "system_header_change"
                                    | "system_purpose_change"
                                    | "system_displayname_change"
                            )
                        ) || LAB_TRACES
                            .iter()
                            .any(|t| p["message"].as_str().unwrap_or_default().starts_with(t))
                    })
                    .filter_map(|p| p["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        for post in notices {
            self.api
                .call(&HttpRequest::new(
                    Method::Delete,
                    format!("/api/v4/posts/{post}"),
                ))
                .await?;
        }
        Ok(())
    }

    /// Archive a channel, as the admin: what sends everybody a
    /// `channel_deleted`.
    pub async fn archive(&self, channel: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        self.api
            .call(&HttpRequest::new(
                Method::Delete,
                format!("/api/v4/channels/{id}"),
            ))
            .await
            .map(|_| ())
    }

    /// Bring an archived channel back, so a test that archives one can
    /// be run twice.
    pub async fn restore(&self, channel: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        self.api
            .call(&HttpRequest::new(
                Method::Post,
                format!("/api/v4/channels/{id}/restore"),
            ))
            .await
            .map(|_| ())
    }

    /// Put the account back into a channel it was taken out of, so a test
    /// that leaves one can be run twice.
    /// Take a mute off a channel, for a lab kept across runs where a
    /// scenario muted it and did not get as far as unmuting.
    pub async fn unmute(&self, channel: &str) -> Result<()> {
        let id = self.channel(channel).await?;
        self.api
            .call(
                &HttpRequest::new(
                    Method::Put,
                    format!("/api/v4/channels/{id}/members/{}/notify_props", self.me),
                )
                .json(&json!({
                    "user_id": self.me,
                    "channel_id": id,
                    "mark_unread": "all",
                })),
            )
            .await
            .map(|_| ())
    }

    pub async fn rejoin(&self, channel: &str) -> Result<()> {
        if self.am_in(channel).await? {
            return Ok(());
        }
        let id = self.channel(channel).await?;
        self.api
            .call(
                &HttpRequest::new(Method::Post, format!("/api/v4/channels/{id}/members"))
                    .json(&json!({ "user_id": self.me })),
            )
            .await
            .map(|_| ())
    }
}

/// Channels whose only purpose is to be numerous.
///
/// One message each, because a channel with nothing in it is not warmed
/// and so would not be measuring anything.
async fn bulk_channels(admin: &Api, team: &str, how_many: usize) -> Result<()> {
    let mut made = 0;
    for n in 0..how_many {
        let name = format!("bulk-{n:03}");
        let channel =
            find_or_make_channel(admin, team, &name, &format!("Bulk {n:03}"), false).await?;
        if admin.written_posts(&channel).await? == 0 {
            admin
                .call(&post(&channel, &format!("Something in {name}."), None))
                .await?;
            made += 1;
        }
    }
    println!("{how_many} bulk channels ({made} newly filled)");
    Ok(())
}

/// The URL name of the channel with a lot of scrollback in it.
pub const LONG: &str = "long";
/// And what the sidebar calls it.
pub const LONG_LABEL: &str = "Long";

/// One channel with enough history to scroll through.
///
/// Scrolling is the one thing a handful of messages cannot show anything
/// about: a list of six rows never recycles a widget, and recycling is
/// where the cost is. Messages are a mix of shapes on purpose -- plain
/// lines, a code block, a list, a long paragraph -- because they do not
/// cost the same to draw and a channel of one shape would measure one
/// shape.
async fn long_channel(
    admin: &Api,
    team: &str,
    tokens: &std::collections::BTreeMap<&str, String>,
    how_many: usize,
) -> Result<()> {
    let channel = find_or_make_channel(admin, team, LONG, LONG_LABEL, false).await?;
    // Everybody who is going to speak has to be in it first; the server
    // answers a post from a non-member with a flat 403.
    for person in PEOPLE {
        let found = admin
            .call(&HttpRequest::get(format!(
                "/api/v4/users/username/{}",
                person.username
            )))
            .await?;
        admin
            .create(
                &HttpRequest::new(Method::Post, format!("/api/v4/channels/{channel}/members"))
                    .json(&json!({ "user_id": id_of(&found)? })),
            )
            .await?;
    }
    let already = admin.written_posts(&channel).await?;
    if already >= how_many {
        println!("{LONG} already has {already} messages");
        return Ok(());
    }

    // Round robin through the seeded people, so author grouping and avatar
    // changes happen the way they do in a real channel.
    let mut voices = Vec::new();
    for token in tokens.values() {
        voices.push(Api::as_token(token)?);
    }
    if voices.is_empty() {
        voices.push(Api::as_token("")?);
    }

    // Reactions on most messages and a picture on some: a seeded
    // channel of plain one-line text binds its rows twenty times faster
    // than a real one, and a fixture that cannot reproduce the cost
    // cannot be used to measure a fix for it.
    let swatch = swatch_png();
    let shapes: &[&str] = &[
        "Line {n} of the long channel, which is here to be scrolled through.",
        "Shorter one. {n}",
        "A longer paragraph, number {n}, with enough words in it to wrap onto a \
         second line in the usual window width, because a message that wraps is \
         measured differently from one that does not and the difference is the \
         whole point of having a mix.",
        "```rust\nlet n = {n};\nassert!(n > 0);\n```",
        // The shape that turned out to matter: a dump pasted into a
        // message. A client that renders one as a single wrapped label
        // of several thousand characters pays for it on every redraw,
        // and a channel full of them took five seconds to open.
        "{dump}",
        "- first of {n}\n- second\n- third",
        "**Bold {n}**, _italic_, `code`, and a [link](https://example.com/{n}).",
        // Mentions are drawn as chips, not text, and a real channel is
        // full of them.
        "@ada could you look at {n}? cc @grace @margaret -- and @here if nobody else",
        "> somebody said {n}\n\nand this is the answer to it, with a :lab: in it",
    ];

    println!("filling {LONG} to {how_many} messages (from {already})");
    let me = id_of(&admin.call(&HttpRequest::get("/api/v4/users/me")).await?)?;
    for n in already..how_many {
        let who = &voices[n % voices.len()];
        let text = shapes[n % shapes.len()]
            .replace("{n}", &n.to_string())
            .replace("{dump}", &dump(n));

        // Every fifth message carries a picture, which is the shape that
        // makes a row expensive to draw.
        let body = if n % 5 == 0 {
            let upload = who
                .call(&mm_api::rest::upload_file(
                    &mm_types::ChannelId::from(channel.clone()),
                    "swatch.png",
                    swatch.clone(),
                ))
                .await?;
            let file = upload["file_infos"][0]["id"].as_str().unwrap_or_default();
            HttpRequest::new(Method::Post, "/api/v4/posts").json(&json!({
                "channel_id": channel,
                "message": text,
                "file_ids": [file],
            }))
        } else {
            post(&channel, &text, None)
        };
        let posted = who.call(&body).await?;

        // A thread under every eighth message: with collapsed threads
        // the replies are not in the channel, but the root grows a
        // "2 replies" button, and a real channel has one every few rows.
        if n % 8 == 0 {
            for (k, other) in voices.iter().enumerate().take(2) {
                other
                    .call(&post(
                        &channel,
                        &format!("reply {k} to {n}"),
                        Some(&id_of(&posted)?),
                    ))
                    .await?;
            }
        }

        // And most of them carry reactions, which are rebuilt on every
        // bind and so are part of what a bind costs.
        if n % 3 != 0 {
            // `lab` is the server's own custom emoji, and a chip for one
            // is a picture rather than a glyph -- a different and much
            // more expensive path. A fixture of standard emoji only
            // measured the cheap half.
            for emoji in ["eyes", "lab", "+1", "tada", "lab"].iter().take(2 + n % 4) {
                admin
                    .create(
                        &HttpRequest::new(Method::Post, "/api/v4/reactions").json(&json!({
                            "user_id": me,
                            "post_id": id_of(&posted)?,
                            "emoji_name": emoji,
                        })),
                    )
                    .await?;
            }
        }
    }
    println!("{LONG} now has {how_many} messages");
    Ok(())
}

/// How big a seeded profile picture is.
///
/// What a real server serves, and the whole point: an avatar arrives
/// much larger than it is drawn, and whether the client scales it once
/// or on every row is the difference between a row costing 0.3ms and
/// several. A lab of users with no picture at all measured neither.
const FACE_PIXELS: usize = 256;

/// Give everybody a profile picture.
async fn give_faces(admin: &Api, ids: &std::collections::BTreeMap<&str, String>) -> Result<()> {
    for (n, (_, id)) in ids.iter().enumerate() {
        let req = HttpRequest::new(Method::Post, format!("/api/v4/users/{id}/image")).multipart(
            Vec::new(),
            "image",
            "face.png",
            face_png(n),
        );
        admin.create(&req).await?;
    }
    println!("{} profile pictures", ids.len());
    Ok(())
}

/// A plain square in a colour of its own, encoded as a PNG.
///
/// Generated rather than committed: the bytes do not matter, only that
/// there are enough of them and that each person's differ.
fn face_png(seed: usize) -> Vec<u8> {
    let (r, g, b) = (
        u8::try_from((seed * 71) % 256).unwrap_or(0),
        u8::try_from((seed * 149) % 256).unwrap_or(0),
        u8::try_from((seed * 223) % 256).unwrap_or(0),
    );
    let mut pixels = Vec::with_capacity(FACE_PIXELS * FACE_PIXELS * 3);
    for y in 0..FACE_PIXELS {
        for x in 0..FACE_PIXELS {
            // A gradient, so the encoder cannot collapse it to nothing
            // and the decoder has real work to do.
            let shade = u8::try_from((x + y) % 256).unwrap_or(0);
            pixels.extend_from_slice(&[r ^ shade, g ^ shade, b ^ shade]);
        }
    }
    let mut out = Vec::new();
    {
        let size = u32::try_from(FACE_PIXELS).unwrap_or(1);
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(&pixels);
        }
    }
    out
}

/// A pasted request/response dump, the way somebody debugging posts one.
///
/// Sixty lines of it, which is what a real one looks like and what a
/// seeded three-line snippet does not measure at all.
fn dump(n: usize) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("```\n");
    let _ = writeln!(
        out,
        "---- REQUEST {n} ----\nPOST /login HTTP/1.1\nHOST : localhost\nHEADERS:"
    );
    for header in 0..18 {
        let _ = writeln!(
            out,
            "        X-Some-Header-{header:02}: application/json; charset=utf-8; boundary=----Boundary{n}{header}"
        );
    }
    out.push_str("BODY :\n{\n");
    for field in 0..14 {
        let _ = writeln!(
            out,
            "    \"field_{field:02}\": \"crm-admin2x933e3a-c7b5-49fd-a53f-043301602090@example.test\","
        );
    }
    out.push_str("}\n---- RESPONSE ----\nSTATUS : 500 Internal Server Error\n");
    for line in 0..12 {
        let _ = writeln!(
            out,
            "    Trace-{line:02}: 6a61eca766951c246b0bbf34 at 2026-07-23T13:27:55.240406408+03:00"
        );
    }
    out.push_str("```");
    out
}

/// How every message the lab writes begins, so a sweep knows its own.
pub const LAB_TRACES: &[&str] = &[
    // A forwarded message is its permalink said again.
    "http://localhost:8065/lab/pl/",
    "Sent from the lab",
    "Edited from the lab",
    "Replied from the lab",
    "@legion something new in Design.",
    "A fresh message, ",
    "Draft with a file",
];

/// A moment, for a message that must differ from the last run's.
#[must_use]
pub fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// The root post in a channel page whose text starts a given way.
fn find_root(page: &Value, starts_with: &str) -> Result<Value> {
    page["posts"]
        .as_object()
        .and_then(|posts| {
            posts.values().find(|p| {
                p["root_id"].as_str().unwrap_or_default().is_empty()
                    && p["message"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with(starts_with)
            })
        })
        .cloned()
        .ok_or_else(|| Error(format!("no root starting with {starts_with:?}")))
}
