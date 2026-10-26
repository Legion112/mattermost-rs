//! Emoji shortcodes.
//!
//! Mattermost writes emoji as `:tada:`, and a client that does not translate
//! them shows colons to a user who typed a picture. The table is GitHub's,
//! which is the one Mattermost's own picker is built from.
//!
//! A shortcode with no match is **left exactly as typed**. Servers carry
//! custom emoji — `:shipit:`, a team's own uploads — and rendering those as
//! an empty string would delete content; `:shipit:` at least says what was
//! meant.

/// The characters a shortcode may contain. Mattermost's own validation is
/// lowercase alphanumerics plus these three, and `+1` / `-1` are real
/// shortcodes, so the set cannot be narrowed.
fn is_shortcode_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+')
}

/// The glyph for a bare shortcode name, without its colons.
#[must_use]
pub fn glyph(name: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(name).map(emojis::Emoji::as_str)
}

/// Replace every `:shortcode:` in `text` with its glyph.
///
/// Scans rather than splits on `:`, because the colon that ends one
/// shortcode can begin the next — `:tada::tada:` is two, and a splitter
/// turns it into one and a stray colon.
#[must_use]
pub fn substitute(text: &str) -> String {
    if !text.contains(':') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(':') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after
            .char_indices()
            .find(|(_, c)| !is_shortcode_char(*c))
            .map_or(after.len(), |(i, _)| i);
        let name = &after[..name_len];
        // An empty `::` is not a shortcode, and neither is anything the
        // table does not know.
        let resolved = after[name_len..]
            .strip_prefix(':')
            .and_then(|tail| glyph(name).map(|g| (g, tail)));
        if let Some((glyph, tail)) = resolved {
            out.push_str(glyph);
            rest = tail;
        } else {
            // Not a shortcode. Consume the colon and carry on, so the next
            // one still gets its chance to open one.
            out.push(':');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[path = "tests/emoji.rs"]
mod tests;
