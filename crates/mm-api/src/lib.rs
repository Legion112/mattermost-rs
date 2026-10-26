//! Mattermost REST v4 and WebSocket protocol.
//!
//! **Inside the coverage gate, at 100%.** Nothing here performs I/O: request
//! builders return a plain `HttpRequest`, response parsers take an
//! `HttpResponse`, and the WebSocket is a state machine fed frames as
//! strings. The sockets live in `mm-http`. That split is what makes every
//! route, every query-parameter edge case and every error path a table test.
