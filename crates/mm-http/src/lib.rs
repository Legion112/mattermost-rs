//! Async transport adapters for `mm-api`.
//!
//! **Outside the coverage gate.** Deliberately thin and boring: this crate
//! turns an [`HttpRequest`] into a `reqwest` call and a WebSocket into a
//! stream of frames, and makes no decisions. Everything that could be decided
//! — which route, what the response means, when to reconnect, how long to wait
//! — is pure and lives in `mm-api`. This crate is meant to be read rather than
//! tested.

use futures_util::{SinkExt, StreamExt};
use mm_api::http::{Body, HttpRequest, HttpResponse, Method};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

/// Something went wrong below the protocol: DNS, TCP, TLS, a closed socket.
///
/// Distinct from `mm_api::ApiError`, which is the server answering. The
/// difference matters to the caller: a transport error is usually worth
/// retrying with backoff, while most API errors are not.
#[derive(Debug)]
pub enum TransportError {
    /// The server URL could not be parsed or joined.
    Url(String),
    /// The request never completed.
    Network(String),
    /// The WebSocket closed or misbehaved.
    Socket(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(d) => write!(f, "bad server URL: {d}"),
            Self::Network(d) => write!(f, "network error: {d}"),
            Self::Socket(d) => write!(f, "websocket error: {d}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// An HTTP client bound to one server.
#[derive(Debug, Clone)]
pub struct Client {
    base: Url,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    /// `base` is the server root, e.g. `https://mattermost.example.com`.
    pub fn new(base: &str) -> Result<Self, TransportError> {
        // A missing scheme is the most common way this is typed wrong, and
        // guessing https is both safer and almost always right.
        let with_scheme = if base.contains("://") {
            base.to_owned()
        } else {
            format!("https://{base}")
        };
        let base = Url::parse(with_scheme.trim_end_matches('/'))
            .map_err(|e| TransportError::Url(format!("{with_scheme}: {e}")))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("mattermost-rs/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| TransportError::Network(e.to_string()))?;
        Ok(Self {
            base,
            token: None,
            http,
        })
    }

    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    #[must_use]
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Fetch bytes from a URL that is not this server.
    ///
    /// A link preview's image is hosted by whoever the link points at, so
    /// this deliberately builds the request by hand and **never attaches
    /// the session token**: sending a Mattermost credential to an
    /// arbitrary third party because somebody pasted a URL would be a
    /// security bug, not a convenience. Only `https` is followed, for the
    /// same reason the rest of the client refuses plain HTTP.
    pub async fn fetch_public(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let parsed = Url::parse(url).map_err(|e| TransportError::Url(format!("{url}: {e}")))?;
        if parsed.scheme() != "https" {
            return Err(TransportError::Url(format!("{url}: not https")));
        }
        let response = self
            .http
            .get(parsed)
            .send()
            .await
            .map_err(|e| TransportError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TransportError::Network(format!(
                "{url}: {}",
                response.status()
            )));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| TransportError::Network(e.to_string()))
    }

    /// Perform one request built by `mm-api`.
    ///
    /// Note what is *not* sent: no cookie jar. The server checks the
    /// `MMAUTHTOKEN` cookie **before** the `Authorization` header, so a stray
    /// cookie would silently win over the token we mean to use.
    pub async fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut url = self
            .base
            .join(&req.path)
            .map_err(|e| TransportError::Url(format!("{}: {e}", req.path)))?;
        if !req.query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in &req.query {
                pairs.append_pair(key, value);
            }
        }

        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        };

        let mut builder = self.http.request(method, url);
        if let Some(token) = &self.token {
            builder = builder.bearer_auth(token);
        }
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        builder = match &req.body {
            Body::Empty => builder,
            Body::Json(json) => builder
                .header("Content-Type", "application/json")
                .body(json.clone()),
            Body::Multipart {
                fields,
                file_field,
                filename,
                content,
            } => {
                let mut form = reqwest::multipart::Form::new();
                for (name, value) in fields {
                    form = form.text(name.clone(), value.clone());
                }
                let part =
                    reqwest::multipart::Part::bytes(content.clone()).file_name(filename.clone());
                builder.multipart(form.part(file_field.clone(), part))
            }
        };

        let resp = builder
            .send()
            .await
            .map_err(|e| TransportError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Network(e.to_string()))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    /// The WebSocket URL for this server, with reconnect parameters applied.
    pub fn websocket_url(&self, query: &[(String, String)]) -> Result<Url, TransportError> {
        let mut url = self
            .base
            .join("/api/v4/websocket")
            .map_err(|e| TransportError::Url(e.to_string()))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|()| TransportError::Url(format!("cannot use {scheme} with this URL")))?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }
}

/// A connected WebSocket.
pub struct WsConn {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl std::fmt::Debug for WsConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsConn")
    }
}

impl WsConn {
    /// Connect, sending the token in the `Authorization` header.
    ///
    /// The header is used rather than the `authentication_challenge` frame
    /// because it fails earlier and more clearly: a rejected challenge closes
    /// the socket with no error frame at all, so "closed immediately" is the
    /// only signal available.
    pub async fn connect(url: &Url, token: &str) -> Result<Self, TransportError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| TransportError::Url(e.to_string()))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| TransportError::Url("token is not a valid header value".into()))?,
        );

        let (stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| TransportError::Socket(e.to_string()))?;
        Ok(Self { stream })
    }

    /// The next text frame, or `None` when the socket closed.
    ///
    /// Ping and pong frames are answered by the library and skipped here. The
    /// server pings every 60s and closes after 100s without a pong, so this
    /// must keep being polled even when the application has nothing to do —
    /// otherwise the connection dies while looking healthy.
    pub async fn next_text(&mut self) -> Option<Result<String, TransportError>> {
        loop {
            match self.stream.next().await? {
                Ok(Message::Text(text)) => return Some(Ok(text.as_str().to_owned())),
                Ok(Message::Close(_)) => return None,
                // Binary, ping and pong frames carry nothing this client
                // reads; the library answers pings itself. Keep polling.
                Ok(_) => {}
                Err(e) => return Some(Err(TransportError::Socket(e.to_string()))),
            }
        }
    }

    pub async fn send_text(&mut self, text: String) -> Result<(), TransportError> {
        self.stream
            .send(Message::text(text))
            .await
            .map_err(|e| TransportError::Socket(e.to_string()))
    }
}
