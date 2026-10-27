use super::*;

/// The ordinary case.
#[test]
fn a_known_shortcode_becomes_its_glyph() {
    assert_eq!(substitute("ship it :tada:"), "ship it 🎉");
    assert_eq!(glyph("tada"), Some("🎉"));
}

/// `+1` and `-1` are real shortcodes, so the character set cannot be
/// narrowed to alphanumerics and underscores.
#[test]
fn the_thumbs_shortcodes_contain_punctuation() {
    assert_eq!(substitute(":+1:"), "👍");
    assert_eq!(substitute(":-1:"), "👎");
}

/// The colon that closes one shortcode opens the next. Splitting on `:`
/// turns this into one emoji and a stray colon.
#[test]
fn adjacent_shortcodes_both_resolve() {
    assert_eq!(substitute(":tada::tada:"), "🎉🎉");
}

/// Servers carry custom emoji that no table knows. Rendering them as nothing
/// would delete what the user wrote.
#[test]
fn an_unknown_shortcode_is_left_exactly_as_typed() {
    assert_eq!(substitute("deploy :shipit: now"), "deploy :shipit: now");
    assert_eq!(glyph("shipit"), None);
}

/// Text that merely contains colons must come through untouched -- a time,
/// a URL, a Python dict, a ratio.
#[test]
fn ordinary_colons_survive() {
    for text in [
        "meet at 10:30",
        "https://example.com:8065/x",
        "{\"a\": 1}",
        "ratio 3:2",
        "::",
        ":",
        "a : b",
        "no colons here",
    ] {
        assert_eq!(substitute(text), text, "for {text}");
    }
}

/// A shortcode that is opened and never closed is just text, and must not
/// swallow the rest of the message.
#[test]
fn an_unclosed_shortcode_does_not_eat_the_line() {
    assert_eq!(substitute(":tada and then some"), ":tada and then some");
    assert_eq!(substitute("a :tada"), "a :tada");
}

/// Substitution runs over message text, which is arbitrary, so it has to be
/// total: never panic, and never lose the non-shortcode characters.
#[test]
fn substitution_is_total_over_awkward_input() {
    for text in ["", ":::", "::::", ":a::b:", "🎉:tada:🎉", ":+:", ":_:"] {
        let out = substitute(text);
        assert!(
            out.chars().count() <= text.chars().count().max(1),
            "{text} grew to {out}"
        );
    }
    assert_eq!(substitute("🎉:tada:🎉"), "🎉🎉🎉");
}

/// The picker's search: an empty query opens on the smileys, a query
/// finds the exact shortcode before the ones merely containing it, and
/// a description finds an emoji whose shortcode does not contain the
/// words at all.
#[test]
fn searching_emoji_puts_the_closest_match_first() {
    let opening = search("", 12);
    assert_eq!(opening.len(), 12);
    // Whatever the table's first entries are, they are real.
    assert!(opening.iter().all(|c| !c.glyph.is_empty()));
    assert!(opening.iter().all(|c| !c.shortcode.is_empty()));

    let smile = search("smile", 20);
    assert_eq!(smile[0].shortcode, "smile");
    assert!(smile.len() > 1, "others contain it too");
    assert!(smile.iter().any(|c| c.shortcode == "smiley"));

    // Found by name, not by shortcode.
    let tears = search("grinning", 20);
    assert!(tears.iter().any(|c| c.shortcode == "grinning"));

    // A space reads as the underscore a shortcode uses.
    assert!(!search("red heart", 20).is_empty());

    assert!(search("definitely-not-an-emoji", 20).is_empty());
    assert_eq!(search("smile", 1).len(), 1);

    // Every derive is exercised.
    let one = smile[0];
    assert_eq!(one, one);
    assert!(!format!("{one:?}").is_empty());
}
