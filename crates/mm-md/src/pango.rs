//! Rendering inlines to Pango markup.
//!
//! Pango parses markup as XML. Malformed markup does not render badly — GTK
//! logs an error and **drops the text entirely**, so a message containing a
//! bare `<` or `&` would silently vanish. Every text path therefore goes
//! through [`escape`], and a property test asserts the result stays
//! well-formed for arbitrary input, including deliberately hostile input.

use crate::parse::Inline;

/// Escape text for inclusion in Pango markup.
///
/// The five XML predefined entities. Apostrophe and quote are escaped too:
/// they are harmless in element content but not in an attribute value, and
/// escaping unconditionally means a future change that moves text into an
/// attribute cannot introduce a hole.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render inlines to Pango markup.
#[must_use]
pub fn to_pango(inlines: &[Inline]) -> String {
    let mut out = String::new();
    render(inlines, &mut out);
    out
}

fn render(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => out.push_str(&escape(text)),
            // Back to what was typed. Pango markup cannot carry an
            // image, so a client drawing a custom emoji's picture does
            // it by reading the inlines itself; this is the fallback,
            // and `:party_parrot:` at least says what was meant.
            Inline::Shortcode(name) => {
                out.push(':');
                out.push_str(&escape(name));
                out.push(':');
            }
            Inline::Code(code) => {
                out.push_str("<tt>");
                out.push_str(&escape(code));
                out.push_str("</tt>");
            }
            Inline::Emphasis(inner) => wrap("i", inner, out),
            Inline::Strong(inner) => wrap("b", inner, out),
            Inline::Strike(inner) => wrap("s", inner, out),
            Inline::Link { href, text } => {
                // The href lands in an attribute, which is exactly where an
                // unescaped quote would break the markup and lose the message.
                out.push_str("<a href=\"");
                out.push_str(&escape(href));
                out.push_str("\">");
                out.push_str(&escape(text));
                out.push_str("</a>");
            }
            Inline::Mention { name, is_me } => {
                // Mattermost's own colours rather than the GTK accent, so a
                // mention reads the same here as in every other client the
                // reader has open. The highlight on their own mention is the
                // one piece of colour a chat window must not get wrong.
                if *is_me {
                    out.push_str(
                        "<span background=\"#ffd470\" foreground=\"#1a1a1a\" weight=\"bold\">",
                    );
                } else {
                    out.push_str("<span foreground=\"#5d89ea\" weight=\"bold\">");
                }
                out.push('@');
                out.push_str(&escape(name));
                out.push_str("</span>");
            }
            Inline::SoftBreak => out.push('\n'),
        }
    }
}

fn wrap(tag: &str, inner: &[Inline], out: &mut String) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    render(inner, out);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

#[cfg(test)]
#[path = "tests/pango.rs"]
mod tests;
