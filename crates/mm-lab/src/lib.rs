//! Docker integration lab.
//!
//! **Outside the coverage gate**, and nothing here is reached by `cargo test
//! --workspace`: the lab needs a Docker daemon, which CI for this repository
//! does not have.
//!
//! What it is for: developing against a Mattermost that nobody else is in.
//! Several features cannot honestly be tested on a server with colleagues on
//! it — leaving a channel posts a system message, adding somebody to a
//! channel notifies them, deleting a channel is forever. Those were the
//! features that kept getting written and not verified. Here they can be run
//! for real, repeatedly, against a server that is thrown away afterwards.
//!
//! Three pieces:
//!
//! - [`stack`] brings a Postgres and a Mattermost up on a private bridge
//!   network, published on loopback only, and waits on a readiness ladder
//!   rather than a sleep.
//! - [`seed`] fills the fresh server with a team, five colleagues, a handful
//!   of channels, a thread, a reaction, an attachment and a custom emoji —
//!   over the same `mm_api` builders the client uses, so a route the server
//!   has changed breaks the seed in the same place it breaks the client.
//! - The `mmlab` binary drives both, and launches the client against the lab
//!   with its own `XDG_STATE_HOME` so the session you use all day is
//!   untouched.
//!
//! The containers are Rust values rather than a compose file so that the
//! definitions, the readiness probe and the seeding cannot drift apart from
//! each other.

mod docker;
pub mod seed;
pub mod stack;
pub mod watch;

/// Anything that went wrong in the lab, as a sentence worth printing.
///
/// One string rather than an enum: every caller is a command-line tool that
/// prints the message and exits, so a variant nobody matches on would be
/// ceremony.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
