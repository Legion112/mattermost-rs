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

/// One run of a message, as the shortcode scanner sees it.
///
/// Borrowed from the input: scanning a message happens once per post
/// drawn, and copying every run of ordinary text to describe it would
/// be most of the cost of the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'a> {
    /// Ordinary text with no shortcode in it.
    Text(&'a str),
    /// A `:shortcode:` the table knows, already resolved.
    Glyph(&'static str),
    /// A well-formed `:shortcode:` with no glyph behind it: a server's
    /// own custom emoji, or a name nothing has. Kept apart from `Text`
    /// so a client that can fetch the picture knows where to put one,
    /// and so one that cannot still has the name to print.
    Unknown(&'a str),
}

/// Split `text` into runs at every `:shortcode:`.
///
/// Scans rather than splits on `:`, because the colon that ends one
/// shortcode can begin the next -- `:tada::tada:` is two, and a
/// splitter turns it into one and a stray colon.
#[must_use]
pub fn scan(text: &str) -> Vec<Piece<'_>> {
    if !text.contains(':') {
        return vec![Piece::Text(text)];
    }
    let mut out: Vec<Piece<'_>> = Vec::new();
    // Where the current run of ordinary text began, in `text`.
    let mut plain = 0;
    let mut at = 0;
    while let Some(offset) = text[at..].find(':') {
        let open = at + offset;
        let after = &text[open + 1..];
        let name_len = after
            .char_indices()
            .find(|(_, c)| !is_shortcode_char(*c))
            .map_or(after.len(), |(i, _)| i);
        let name = &after[..name_len];
        // An empty `::` is not a shortcode, and neither is one that is
        // never closed.
        if name.is_empty() || !after[name_len..].starts_with(':') {
            // Consume the colon and carry on, so the next one still
            // gets its chance to open one.
            at = open + 1;
            continue;
        }
        if plain < open {
            out.push(Piece::Text(&text[plain..open]));
        }
        out.push(glyph(name).map_or(Piece::Unknown(name), Piece::Glyph));
        at = open + 1 + name_len + 1;
        plain = at;
    }
    if plain < text.len() {
        out.push(Piece::Text(&text[plain..]));
    }
    out
}

#[cfg(test)]
#[path = "tests/emoji.rs"]
mod tests;
