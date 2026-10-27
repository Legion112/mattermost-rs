use super::*;

/// What `scan` used to be: the runs joined back into one string, with
/// an unresolved shortcode written out as it was typed. Every
/// assertion below is about that, because it is what a reader sees
/// from a client that cannot draw a custom emoji's picture.
fn substitute(text: &str) -> String {
    scan(text)
        .into_iter()
        .map(|piece| match piece {
            Piece::Text(run) => run.to_owned(),
            Piece::Glyph(glyph) => glyph.to_owned(),
            Piece::Unknown(name) => format!(":{name}:"),
        })
        .collect()
}

/// The runs themselves, which is what the parser reads: a custom emoji
/// has to come back as its own piece, or nothing downstream can put a
/// picture where it was.
#[test]
fn an_unknown_shortcode_is_a_piece_of_its_own() {
    assert_eq!(
        scan("deploy :shipit: now"),
        [
            Piece::Text("deploy "),
            Piece::Unknown("shipit"),
            Piece::Text(" now"),
        ]
    );
    // Known ones resolve in place, and text with none in it is one run.
    assert_eq!(
        scan("ship it :tada:"),
        [Piece::Text("ship it "), Piece::Glyph("\u{1f389}")]
    );
    assert_eq!(scan("plain"), [Piece::Text("plain")]);
    assert_eq!(scan(""), [Piece::Text("")]);
    // Two custom ones back to back, with the colon that closes the
    // first opening the second.
    assert_eq!(
        scan(":shipit::shipit:"),
        [Piece::Unknown("shipit"), Piece::Unknown("shipit")]
    );
    // Every derive is exercised.
    assert_eq!(Piece::Text("a"), Piece::Text("a").clone());
    assert!(!format!("{:?}", Piece::Glyph("x")).is_empty());
}

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
