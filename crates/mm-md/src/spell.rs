//! Which parts of a half-typed message are worth spell-checking.
//!
//! The checking itself needs dictionaries off the disk and belongs to the
//! window. **Deciding what counts as a word does not**, and it is where
//! all the judgement is: a message is full of things that are spelled
//! exactly as intended and are in no dictionary -- `@legion`,
//! `PP-14869`, `postback_gate`, `https://jira.webtime.tech/browse/…`,
//! `:tada:`, and whatever is between backticks. Underlining those would
//! make the composer unreadable in a channel like this one, where most
//! messages are mostly identifiers.
//!
//! So this returns spans, in character offsets, and the window asks its
//! dictionaries about each one.

/// One run of letters, and where it sits in the text.
///
/// Offsets are **characters, not bytes**, because that is what a GTK
/// text buffer counts in and the alternative is a conversion at every
/// call site on text that is routinely Cyrillic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// The words in `text` that a dictionary should be asked about.
#[must_use]
pub fn words(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().collect();
    let code = code_mask(&chars);
    let mut found = Vec::new();
    for (start, end) in tokens(&chars, &code) {
        if !is_checkable(&chars[start..end]) {
            continue;
        }
        letter_runs(&chars, start, end, &mut found);
    }
    found
}

/// Which characters are inside code, and so not prose.
///
/// A run of three or more backticks opens and closes a fence; a shorter
/// run opens and closes an inline span. An opener with no closer runs to
/// the end, which is the common case while a message is still being
/// typed -- and treating the tail as code is the quiet failure, where
/// treating it as prose would underline the half-written snippet.
fn code_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut fenced = false;
    let mut inline = false;
    let mut at = 0;
    while at < chars.len() {
        if chars[at] != '`' {
            mask[at] = fenced || inline;
            at += 1;
            continue;
        }
        let mut run = at;
        while run < chars.len() && chars[run] == '`' {
            run += 1;
        }
        if run - at >= 3 {
            fenced = !fenced;
            inline = false;
        } else if !fenced {
            inline = !inline;
        }
        for flag in &mut mask[at..run] {
            *flag = true;
        }
        at = run;
    }
    mask
}

/// Split into whitespace-separated tokens, skipping anything in code.
fn tokens(chars: &[char], code: &[bool]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for at in 0..=chars.len() {
        let breaks = at == chars.len() || chars[at].is_whitespace() || code[at];
        match (breaks, start) {
            (true, Some(from)) => {
                spans.push((from, at));
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    spans
}

/// Whether a whole token is prose at all.
///
/// Judged on the token rather than on the letter runs inside it, because
/// that is the only level at which `PP-14869` is one thing: by the time
/// it has been split on the hyphen, `PP` looks like a word and is not.
fn is_checkable(token: &[char]) -> bool {
    // A digit anywhere makes it an identifier, a version or a ticket.
    if token.iter().any(char::is_ascii_digit) {
        return false;
    }
    // The characters that start or join things that are not prose:
    // mentions, channel links, emoji codes, URLs, paths, snake_case.
    if token
        .iter()
        .any(|c| matches!(c, '@' | '~' | ':' | '/' | '\\' | '_' | '#' | '<' | '>'))
    {
        return false;
    }
    let word: String = token.iter().collect();
    !word.to_lowercase().starts_with("www.")
}

/// Pull the runs of letters out of one token.
fn letter_runs(chars: &[char], start: usize, end: usize, found: &mut Vec<Word>) {
    let mut at = start;
    while at < end {
        if !chars[at].is_alphabetic() {
            at += 1;
            continue;
        }
        let from = at;
        while at < end && (chars[at].is_alphabetic() || chars[at] == '\'') {
            at += 1;
        }
        // An apostrophe is part of "don't" but not of "'quoted'".
        let mut to = at;
        while to > from && chars[to - 1] == '\'' {
            to -= 1;
        }
        let text: String = chars[from..to].iter().collect();
        // One letter is never a misspelling worth drawing, and an
        // acronym -- API, PHP, CDN -- is not one either, however little
        // the dictionary knows about it.
        if text.chars().count() < 2 || is_acronym(&text) {
            continue;
        }
        found.push(Word {
            start: from,
            end: to,
            text,
        });
    }
}

fn is_acronym(word: &str) -> bool {
    word.chars().all(|c| c.is_uppercase() || c == '\'')
}

#[cfg(test)]
#[path = "tests/spell.rs"]
mod tests;
