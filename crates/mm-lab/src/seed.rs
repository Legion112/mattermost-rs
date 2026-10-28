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
struct Api(mm_http::Client);

impl Api {
    fn anonymous() -> Result<Self> {
        mm_http::Client::new(crate::stack::URL)
            .map(Self)
            .map_err(|e| Error(e.to_string()))
    }

    fn as_token(token: &str) -> Result<Self> {
        Ok(Self(
            mm_http::Client::new(crate::stack::URL)
                .map_err(|e| Error(e.to_string()))?
                .with_token(token),
        ))
    }

    /// Send, and fail with the server's own message rather than a status code.
    async fn call(&self, req: &HttpRequest) -> Result<Value> {
        let resp = self
            .0
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
    let found = admin
        .call(&HttpRequest::get(format!(
            "/api/v4/teams/{team}/channels/name/{name}"
        )))
        .await?;
    id_of(&found)
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
        let found = self
            .api
            .call(&HttpRequest::get(format!(
                "/api/v4/teams/{}/channels/name/{name}",
                self.team
            )))
            .await?;
        id_of(&found)
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

    /// Put the account back into a channel it was taken out of, so a test
    /// that leaves one can be run twice.
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
        "- first of {n}\n- second\n- third",
        "**Bold {n}**, _italic_, `code`, and a [link](https://example.com/{n}).",
    ];

    println!("filling {LONG} to {how_many} messages (from {already})");
    let me = id_of(&admin.call(&HttpRequest::get("/api/v4/users/me")).await?)?;
    for n in already..how_many {
        let who = &voices[n % voices.len()];
        let text = shapes[n % shapes.len()].replace("{n}", &n.to_string());

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
