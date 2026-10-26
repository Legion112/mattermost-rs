//! Mattermost wire and domain types.
//!
//! **Outside the coverage gate.** serde derives generate per-field branches
//! attributed to the `#[derive]` line, so a 100% line gate here would mean
//! hand-writing malformed JSON for every field of every type. The control
//! that replaces it is stronger: every public type needs a
//! captured-from-server golden fixture, and `cargo xtask fixtures` fails CI
//! when one is missing. That catches upstream field renames, which is the
//! actual risk.
//!
//! Ids are newtypes, never bare `String`: Mattermost ids are all 26-char
//! base32, so a `ChannelId` passed where a `PostId` belongs is a silent bug
//! that the type system should have caught.
