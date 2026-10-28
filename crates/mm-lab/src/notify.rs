//! A notification server of the lab's own.
//!
//! The container has no desktop, so nobody is listening on
//! `org.freedesktop.Notifications`: a banner the client sends goes
//! nowhere, and a click on one cannot be faked. This takes the name and
//! answers the way a shell does -- an id for each `Notify`, the
//! `ActionInvoked` signal when the lab "clicks" -- so a scenario can see
//! what the client sent and watch what it does when the banner is
//! clicked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use crate::{Error, Result};

/// One banner as the client sent it.
#[derive(Debug, Clone)]
pub struct Banner {
    pub id: u32,
    pub app: String,
    pub icon: String,
    pub timeout: i32,
    pub summary: String,
    pub body: String,
    /// The `x-mattermost-rs-channel` hint, when the client set one.
    pub channel: Option<String>,
    pub actions: Vec<String>,
}

#[derive(Debug, Default)]
struct Server {
    received: Arc<Mutex<Vec<Banner>>>,
    next: Mutex<u32>,
}

// The bus's shape, not this crate's: every method of an interface takes
// `&self`, and `Notify` takes its hints by value, because that is what the
// macro expects to hand it.
#[allow(clippy::unused_self, clippy::needless_pass_by_value)]
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "mmlab".to_owned(),
            "mattermost-rs".to_owned(),
            "1".to_owned(),
            "1.2".to_owned(),
        )
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".to_owned(), "body".to_owned()]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id == 0 {
            let mut next = self
                .next
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *next += 1;
            *next
        } else {
            replaces_id
        };
        let channel = hints
            .get("x-mattermost-rs-channel")
            .and_then(|v| <&str>::try_from(v).ok().map(str::to_owned));
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Banner {
                id,
                app: app_name.to_owned(),
                icon: app_icon.to_owned(),
                timeout: expire_timeout,
                summary: summary.to_owned(),
                body: body.to_owned(),
                channel,
                actions,
            });
        id
    }

    /// A banner the client takes back is gone from the list.
    fn close_notification(&self, id: u32) {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|b| b.id != id);
    }

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

/// The lab's stand-in for the desktop's notification daemon.
#[derive(Debug)]
pub struct Desk {
    connection: zbus::Connection,
    received: Arc<Mutex<Vec<Banner>>>,
}

const PATH: &str = "/org/freedesktop/Notifications";

impl Desk {
    /// Take the name on the session bus. Fails if something else has it,
    /// which on a desktop is the real daemon -- this is for the container.
    pub async fn serve() -> Result<Self> {
        let received = Arc::new(Mutex::new(Vec::new()));
        let server = Server {
            received: Arc::clone(&received),
            next: Mutex::new(0),
        };
        let connection = zbus::connection::Builder::session()
            .map_err(|e| Error(format!("no session bus for notifications: {e}")))?
            .name("org.freedesktop.Notifications")
            .map_err(|e| Error(format!("the notifications name: {e}")))?
            .serve_at(PATH, server)
            .map_err(|e| Error(format!("serving notifications: {e}")))?
            .build()
            .await
            .map_err(|e| Error(format!("taking the notifications name: {e}")))?;
        Ok(Self {
            connection,
            received,
        })
    }

    /// Every banner sent so far, oldest first.
    pub fn banners(&self) -> Vec<Banner> {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Click a banner: its `default` action, then the close that follows
    /// a click on every desktop.
    pub async fn click(&self, id: u32) -> Result<()> {
        let iface = self
            .connection
            .object_server()
            .interface::<_, Server>(PATH)
            .await
            .map_err(|e| Error(format!("the notifications object: {e}")))?;
        Server::action_invoked(iface.signal_emitter(), id, "default")
            .await
            .map_err(|e| Error(format!("clicking the banner: {e}")))?;
        Server::notification_closed(iface.signal_emitter(), id, 2)
            .await
            .map_err(|e| Error(format!("closing the banner: {e}")))
    }
}
