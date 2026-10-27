//! What a search query actually looks for, and showing it in a result.
//!
//! The server can return the matched terms alongside the posts, but
//! only where Elasticsearch is configured: measured against a live
//! 12.0.1 server with the default database search, `matches` comes
//! back `null`. So the terms are picked apart here instead, which has
//! the side benefit of being pure and testable.

/// The words a query is really searching for.
///
/// Mattermost's query language rides in the same string as the words:
/// `from:legion`, `in:town-square`, `before:2026-01-01`, `-excluded`,
/// and `"a quoted phrase"` as one unit. A client that highlighted the
/// modifiers would bold the word `from` in every result.
#[must_use]
pub fn words(terms: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = terms;
    while !rest.is_empty() {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        let (token, tail) = if let Some(body) = trimmed.strip_prefix('"') {
            match body.split_once('"') {
                Some((inside, tail)) => (inside, tail),
                // An unclosed quote is the rest of the query, which is
                // what the server does with one too.
                None => (body, ""),
            }
        } else {
            match trimmed.split_once(char::is_whitespace) {
                Some((token, tail)) => (token, tail),
                None => (trimmed, ""),
            }
        };
        rest = tail;
        if let Some(word) = wanted(token) {
            out.push(word);
        }
    }
    out
}

/// One token, reduced to the word it searches for -- or `None` when it
/// searches for nothing a reader would want pointed at.
fn wanted(token: &str) -> Option<String> {
    // `-word` excludes it, so a result cannot contain it.
    if token.starts_with('-') {
        return None;
    }
    // `from:`, `in:`, `before:` and the rest. Recognised by shape
    // rather than by a list, because the list grows with the server:
    // letters, then a colon, then something.
    if let Some((head, tail)) = token.split_once(':')
        && !head.is_empty()
        && !tail.is_empty()
        && head.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    // A trailing `*` is Mattermost's prefix wildcard; the stem is what
    // actually appears in the text.
    let word = token.trim_end_matches('*').trim();
    (!word.is_empty()).then(|| word.to_lowercase())
}

/// `text` as Pango markup, with every occurrence of `words` in bold.
///
/// Case-insensitively, and over the **characters** of the haystack
/// rather than its bytes, so a Cyrillic query highlights a Cyrillic
/// message rather than slicing one in half.
#[must_use]
pub fn highlight(text: &str, words: &[String]) -> String {
    if words.is_empty() {
        return crate::escape(text);
    }
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let chars: Vec<char> = text.chars().collect();
    // Lowercasing can change the character count -- a Turkish dotted
    // I becomes two -- and an index into one string would then be the
    // wrong place in the other. Rather than mapping between them, this
    // gives up and escapes the lot, which draws the result without the
    // bold rather than drawing it wrong.
    if lower.len() != chars.len() {
        return crate::escape(text);
    }

    let mut bold = vec![false; chars.len()];
    for word in words {
        let needle: Vec<char> = word.to_lowercase().chars().collect();
        if needle.is_empty() || needle.len() > chars.len() {
            continue;
        }
        for at in 0..=chars.len() - needle.len() {
            if lower[at..at + needle.len()] == needle[..] {
                for flag in &mut bold[at..at + needle.len()] {
                    *flag = true;
                }
            }
        }
    }

    let mut out = String::with_capacity(text.len());
    let mut open = false;
    for (at, ch) in chars.iter().enumerate() {
        if bold[at] != open {
            out.push_str(if bold[at] { "<b>" } else { "</b>" });
            open = bold[at];
        }
        out.push_str(&crate::escape(&ch.to_string()));
    }
    if open {
        out.push_str("</b>");
    }
    out
}

#[cfg(test)]
#[path = "tests/search.rs"]
mod tests;
