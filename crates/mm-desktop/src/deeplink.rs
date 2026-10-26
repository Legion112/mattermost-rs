//! `mattermost://` link parsing.
//!
//! Pure string work, so the whole surface is testable without a desktop. The
//! XDG registration that routes such a link to this process lives in
//! `mm-desktop-xdg` and in the `.desktop` file.

use mm_types::PostId;

/// Where a link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    /// `mattermost://host/team/channels/channel-name`
    Channel {
        host: String,
        team: String,
        channel: String,
    },
    /// `mattermost://host/team/pl/<post id>` — a permalink to one message.
    Permalink {
        host: String,
        team: String,
        post: PostId,
    },
    /// `mattermost://host` — just a server.
    Server { host: String },
}

impl DeepLink {
    #[must_use]
    pub fn host(&self) -> &str {
        match self {
            Self::Channel { host, .. } | Self::Permalink { host, .. } | Self::Server { host } => {
                host
            }
        }
    }
}

/// Why a link could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLinkError {
    /// Not a `mattermost://` URL at all.
    WrongScheme(String),
    /// No host, so there is no way to tell which server is meant.
    NoHost,
    /// The path was not a shape this client recognises.
    Unrecognised(String),
}

impl std::fmt::Display for DeepLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongScheme(got) => {
                write!(f, "expected a mattermost:// link, got {got:?}")
            }
            Self::NoHost => f.write_str("the link names no server"),
            Self::Unrecognised(path) => write!(f, "unrecognised link path {path:?}"),
        }
    }
}

impl std::error::Error for DeepLinkError {}

/// Parse a `mattermost://` URL.
///
/// Deliberately hand-rolled rather than going through a URL crate: these links
/// arrive from the desktop environment as an argv string, the grammar is three
/// shapes, and the failure modes we care about are better described here than
/// as a generic parse error.
pub fn parse(raw: &str) -> Result<DeepLink, DeepLinkError> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("mattermost://")
        .ok_or_else(|| DeepLinkError::WrongScheme(trimmed.to_owned()))?;

    // Strip any query or fragment; neither carries routing information.
    let rest = rest
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts.next().ok_or(DeepLinkError::NoHost)?.to_owned();
    let segments: Vec<&str> = parts.collect();

    match segments.as_slice() {
        [] => Ok(DeepLink::Server { host }),
        [team, "channels", channel] => Ok(DeepLink::Channel {
            host,
            team: (*team).to_owned(),
            channel: (*channel).to_owned(),
        }),
        [team, "pl", post] => Ok(DeepLink::Permalink {
            host,
            team: (*team).to_owned(),
            post: PostId::new(*post),
        }),
        _ => Err(DeepLinkError::Unrecognised(rest.to_owned())),
    }
}

#[cfg(test)]
#[path = "tests/deeplink.rs"]
mod tests;
