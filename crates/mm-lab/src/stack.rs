//! The lab stack: a Postgres and a Mattermost on a private bridge network.

use std::time::{Duration, Instant};

use crate::docker::{self, Container};
use crate::{Error, Result};

/// Stamped on every container, volume and network the lab creates, so that
/// `down` can find them all without being told their names.
pub const LABEL: &str = "dev.legion.mattermost_rs.lab=1";

const PREFIX: &str = "mmrs-lab";
const NETWORK: &str = "mmrs-lab";
const DB: &str = "mmrs-lab-db";
const APP: &str = "mmrs-lab-app";

/// Published on loopback. 8065 is Mattermost's own default, so a URL typed
/// from memory works.
pub const PORT: u16 = 8065;

/// What to type into the client's server field.
pub const URL: &str = "http://localhost:8065";

const DB_IMAGE: &str = "postgres:15-alpine";
const APP_IMAGE: &str = "mattermost/mattermost-team-edition:latest";

/// Credentials for the lab database. Hardcoded on purpose: the stack is bound
/// to 127.0.0.1 and holds nothing but fixtures, and a lab whose password has
/// to be found somewhere is a lab nobody starts.
const DB_USER: &str = "mmuser";
const DB_PASS: &str = "mmuser-lab";
const DB_NAME: &str = "mattermost";

/// How long the server gets to come up before the lab gives up and prints its
/// logs. First boot runs every database migration, which on a cold volume is
/// the slow case this has to allow for.
const READY_TIMEOUT: Duration = Duration::from_secs(240);

fn db_container() -> Container {
    Container {
        name: DB.into(),
        image: DB_IMAGE.into(),
        env: vec![
            format!("POSTGRES_USER={DB_USER}"),
            format!("POSTGRES_PASSWORD={DB_PASS}"),
            format!("POSTGRES_DB={DB_NAME}"),
        ],
        ports: Vec::new(),
        volumes: vec![(format!("{PREFIX}-db"), "/var/lib/postgresql/data".into())],
        network: NETWORK.into(),
        alias: DB.into(),
        labels: vec![LABEL.into()],
    }
}

fn app_container() -> Container {
    // Named volumes rather than bind mounts. The server image runs as uid
    // 2000 and refuses to start if it cannot write its data directory; a
    // named volume inherits the image's ownership on first use, while a bind
    // mount from the host arrives owned by whoever ran `up`.
    let volumes = [
        "data",
        "logs",
        "config",
        "plugins",
        "client-plugins",
        "bleve",
    ]
    .into_iter()
    .map(|dir| {
        let at = match dir {
            "bleve" => "/mattermost/bleve-indexes".to_owned(),
            "client-plugins" => "/mattermost/client/plugins".to_owned(),
            other => format!("/mattermost/{other}"),
        };
        (format!("{PREFIX}-{dir}"), at)
    })
    .collect();

    Container {
        name: APP.into(),
        image: APP_IMAGE.into(),
        env: vec![
            "MM_SQLSETTINGS_DRIVERNAME=postgres".into(),
            format!(
                "MM_SQLSETTINGS_DATASOURCE=postgres://{DB_USER}:{DB_PASS}@{DB}:5432/{DB_NAME}\
                 ?sslmode=disable&connect_timeout=10"
            ),
            // Must match how the client reaches it, or the WebSocket origin
            // check rejects the upgrade and permalinks point nowhere.
            format!("MM_SERVICESETTINGS_SITEURL={URL}"),
            "MM_BLEVESETTINGS_INDEXDIR=/mattermost/bleve-indexes".into(),
            // Everything below exists so the lab can exercise features the
            // client has, rather than so the server is realistic. A default
            // install has several of them off.
            "MM_SERVICESETTINGS_ENABLELOCALMODE=true".into(),
            "MM_SERVICESETTINGS_ENABLECUSTOMEMOJI=true".into(),
            "MM_SERVICESETTINGS_ENABLEUSERTYPINGMESSAGES=true".into(),
            "MM_SERVICESETTINGS_ENABLELINKPREVIEWS=true".into(),
            "MM_SERVICESETTINGS_COLLAPSEDTHREADS=default_on".into(),
            "MM_SERVICESETTINGS_POSTEDITTIMELIMIT=-1".into(),
            "MM_SERVICESETTINGS_ENABLEAPICHANNELDELETION=true".into(),
            "MM_FILESETTINGS_ENABLEFILEATTACHMENTS=true".into(),
            "MM_TEAMSETTINGS_ENABLEOPENSERVER=true".into(),
            // No mail server in the lab: asking for verification would make
            // every seeded account unusable.
            "MM_EMAILSETTINGS_REQUIREEMAILVERIFICATION=false".into(),
            "MM_EMAILSETTINGS_SENDEMAILNOTIFICATIONS=false".into(),
            // The default minimum is 8 and the seeded password has to clear
            // it; stated here so the two cannot drift.
            "MM_PASSWORDSETTINGS_MINIMUMLENGTH=8".into(),
        ],
        ports: vec![(PORT, 8065)],
        volumes,
        network: NETWORK.into(),
        alias: APP.into(),
        labels: vec![LABEL.into()],
    }
}

/// Create everything that is missing and start everything that is stopped.
///
/// Idempotent: running `up` on a running lab checks it is ready and returns.
pub async fn up() -> Result<()> {
    docker::require_daemon()?;
    docker::pull(DB_IMAGE)?;
    docker::pull(APP_IMAGE)?;
    docker::network_create(NETWORK, LABEL)?;

    for spec in [db_container(), app_container()] {
        for (volume, _) in &spec.volumes {
            docker::volume_create(volume, LABEL)?;
        }
        match docker::state(&spec.name) {
            Some(true) => println!("{} already running", spec.name),
            Some(false) => {
                println!("starting {}", spec.name);
                docker::start_existing(&spec.name)?;
            }
            None => {
                println!("creating {}", spec.name);
                docker::run(&spec)?;
            }
        }
    }

    wait_ready().await
}

/// Poll the readiness ladder until the server answers for real.
///
/// `GET /system/ping` alone is the wrong probe: it answers `OK` while the
/// database is unreachable, which is exactly the window a freshly created
/// stack spends migrating. `get_server_status=true` adds `database_status`,
/// and that is the fact worth waiting on.
pub async fn wait_ready() -> Result<()> {
    let client = mm_http::Client::new(URL).map_err(|e| Error(e.to_string()))?;
    let started = Instant::now();
    let mut last = String::new();
    let mut said = false;

    while started.elapsed() < READY_TIMEOUT {
        match client.send(&mm_api::rest::ping()).await {
            Ok(resp) => match mm_api::rest::parse_ping(&resp) {
                Ok(ping) if ping.is_ready() => {
                    println!("lab ready at {URL} after {}s", started.elapsed().as_secs());
                    return Ok(());
                }
                Ok(ping) => last = format!("ping says {ping:?}"),
                Err(e) => last = format!("ping did not parse: {e}"),
            },
            Err(e) => last = e.to_string(),
        }
        if !said {
            println!("waiting for the server to finish starting...");
            said = true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Err(Error(format!(
        "the lab server was not ready within {}s. Last probe: {last}\n\nLast lines of {APP}:\n{}",
        READY_TIMEOUT.as_secs(),
        docker::logs(APP, 30).unwrap_or_else(|e| e.to_string())
    )))
}

/// Stop and delete everything carrying the lab label, data included.
pub fn down(keep_data: bool) -> Result<()> {
    docker::require_daemon()?;
    let containers = docker::labelled("container", LABEL)?;
    println!("removing {} container(s)", containers.len());
    docker::remove_containers(&containers)?;

    if !keep_data {
        let volumes = docker::labelled("volume", LABEL)?;
        println!("removing {} volume(s)", volumes.len());
        docker::remove("volume", &volumes)?;
    }

    let networks = docker::labelled("network", LABEL)?;
    docker::remove("network", &networks)?;
    Ok(())
}

/// What is running, in one line each.
pub fn status() -> Result<String> {
    use std::fmt::Write as _;

    docker::require_daemon()?;
    let mut out = String::new();
    for name in [DB, APP] {
        let line = match docker::state(name) {
            Some(true) => "running",
            Some(false) => "stopped",
            None => "absent",
        };
        let _ = writeln!(out, "{name:<14} {line}");
    }
    let _ = writeln!(out, "{:<14} {URL}", "url");
    Ok(out)
}

/// The last lines of the server's log, which is where a failed boot says why.
pub fn logs(lines: u32) -> Result<String> {
    docker::logs(APP, lines)
}
