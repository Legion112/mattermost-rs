//! Markdown with Mattermost extensions.
//!
//! **Inside the coverage gate, at 100%.** `parse` is pure: mention, channel
//! and emoji resolution come in as plain data via the render context, so no
//! part of this crate needs a server or a widget.
