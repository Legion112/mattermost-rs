//! Async transport adapters for `mm-api`.
//!
//! **Outside the coverage gate.** Deliberately thin and boring: reqwest,
//! tokio-tungstenite, TLS, and the timer half of rate limiting. The bucket
//! arithmetic is pure and lives in `mm-api`. This crate is meant to be read,
//! not tested.
