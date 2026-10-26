//! Mattermost REST v4 and WebSocket protocol.
//!
//! **Inside the coverage gate, at 100%.** Nothing here performs I/O: request
//! builders return a plain [`HttpRequest`], response parsers take an
//! [`HttpResponse`], and the WebSocket is a state machine fed frames as
//! strings. The sockets live in `mm-http`.
//!
//! That split is what makes every route, every query-parameter edge case and
//! every error path a table test. It also meant this crate needs no transport
//! trait and no recording fake: a parser's failure modes are reachable by
//! constructing the response that triggers them, which is simpler than
//! scripting a fake transport to return it.
//!
//! The protocol facts encoded here — and *why* they are not obvious — are
//! documented in `docs/protocol.md`, with citations into the server source.

pub mod error;
pub mod http;
pub mod rest;
pub mod ws;

pub use error::{ApiError, AppError};
pub use http::{Body, HttpRequest, HttpResponse, Method};
pub use ws::{WsEvent, WsOutcome, WsProtocol};
