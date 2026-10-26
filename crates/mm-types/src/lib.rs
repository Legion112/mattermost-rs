//! Mattermost wire and domain types.
//!
//! **Outside the coverage gate.** serde derives generate per-field branches
//! attributed to the `#[derive]` line, so a 100% line gate here would mean
//! hand-writing malformed JSON for every field of every type. The control that
//! replaces it is a round-trip and tolerance test per type, which catches the
//! thing that actually goes wrong — an upstream field rename — in a way line
//! coverage never would.
//!
//! Two rules hold throughout, and both exist because the server grows fields
//! between releases:
//!
//! - **`#[serde(default)]` on every struct.** The same model arrives with
//!   different fields populated depending on the endpoint, and a missing field
//!   must never be an error.
//! - **Never `deny_unknown_fields` in production.** A client that refused to
//!   parse a payload because the server added a field would break on the next
//!   minor release. (The drift suite in `mm-lab` does the opposite
//!   deliberately, with mirror structs, so that a shape change is still
//!   noticed.)
//!
//! Ids are newtypes rather than `String`: every Mattermost id is a 26-char
//! base32 string, so passing a `ChannelId` where a `PostId` belongs type-checks
//! unless the types say otherwise.

mod category;
mod channel;
mod ids;
mod post;
mod prefs;
mod time;
mod user;
pub mod wire;

pub use category::{CategoryKind, CategoryList, CategorySorting, ChannelCategory};
pub use channel::{Channel, ChannelMember, ChannelType, MARK_UNREAD, MARK_UNREAD_MENTION};
pub use ids::{ChannelId, EmojiId, FileId, ID_LEN, PostId, TeamId, UserId};
pub use post::{FileRef, Post, PostList};
pub use prefs::{NameFormat, Preference, is_true, keys};
pub use time::Timestamp;
pub use user::{Presence, Team, User};
