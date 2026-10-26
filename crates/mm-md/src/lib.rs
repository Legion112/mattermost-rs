//! Markdown with Mattermost extensions.
//!
//! **Inside the coverage gate, at 100%.** `parse` is pure and `to_pango` is a
//! string transformation, so the whole surface is testable without a widget.
//!
//! The output is an abstract document, not HTML and not a widget tree. The
//! GUI maps blocks onto widgets and inlines onto Pango markup; keeping that
//! mapping out of here is what lets the parser be tested without GTK, and what
//! will let a terminal front-end reuse it unchanged.
//!
//! **Escaping is the dangerous part.** Pango parses its markup as XML, and
//! malformed markup makes GTK log an error and drop the text entirely — so a
//! message containing `<` or `&` would silently vanish rather than render
//! wrong. Every text path goes through [`escape`], and a property test asserts
//! the output stays well-formed for arbitrary input.

mod pango;
mod parse;

pub use pango::{escape, to_pango};
pub use parse::{Block, Document, Inline, parse};
