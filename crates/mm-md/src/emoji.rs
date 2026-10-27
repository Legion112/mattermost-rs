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

/// One emoji offered by the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// The name without colons, which is what a reaction is sent as.
    pub shortcode: &'static str,
    pub glyph: &'static str,
}

/// The emoji a picker should offer for what has been typed.
///
/// An empty query is the start of the table, which is the smileys --
/// the same thing Mattermost opens its picker on, and the ones most
/// reactions come from. A query matches the shortcode first and the
/// emoji's own name second, so typing "smile" finds `:smile:` before
/// `:smiley_cat:`, and typing "grinning face" finds it by description.
///
/// Custom emoji a server has uploaded are not here: this is the
/// built-in table, and a server's own uploads would have to be fetched.
#[must_use]
pub fn search(query: &str, limit: usize) -> Vec<Choice> {
    let wanted = query.trim().to_lowercase().replace(' ', "_");
    let choice = |e: &'static emojis::Emoji| {
        e.shortcode().map(|shortcode| Choice {
            shortcode,
            glyph: e.as_str(),
        })
    };
    if wanted.is_empty() {
        return emojis::iter().filter_map(choice).take(limit).collect();
    }
    let mut best: Vec<(u8, Choice)> = emojis::iter()
        .filter_map(|e| {
            let found = choice(e)?;
            let name = e.name().to_lowercase().replace(' ', "_");
            let tier = if found.shortcode == wanted {
                0
            } else if found.shortcode.starts_with(&wanted) {
                1
            } else if found.shortcode.contains(&wanted) {
                2
            } else if name.contains(&wanted) {
                3
            } else {
                return None;
            };
            Some((tier, found))
        })
        .collect();
    // Stable within a tier, so the table's own order -- which puts the
    // common ones first -- survives.
    best.sort_by_key(|(tier, _)| *tier);
    best.into_iter()
        .map(|(_, found)| found)
        .take(limit)
        .collect()
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
