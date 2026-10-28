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
