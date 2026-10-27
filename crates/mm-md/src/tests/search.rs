use super::*;

/// The query language rides in the same string as the words, and a
/// client that highlighted the modifiers would bold `from` in every
/// result.
#[test]
fn the_modifiers_are_not_words_to_look_for() {
    assert_eq!(words("deploy"), ["deploy"]);
    assert_eq!(words("from:legion deploy"), ["deploy"]);
    assert_eq!(
        words("in:town-square before:2026-01-01 redirector"),
        ["redirector"]
    );
    // An exclusion cannot appear in a result, so there is nothing to
    // point at.
    assert_eq!(words("deploy -staging"), ["deploy"]);
    // A trailing wildcard is the server's; the stem is what appears.
    assert_eq!(words("redirect*"), ["redirect"]);
    // Case is dropped on the way in, because the comparison is
    // case-insensitive and lowering once is cheaper than per result.
    assert_eq!(words("Deploy REPORT"), ["deploy", "report"]);
    assert!(words("   ").is_empty());
    assert!(words("").is_empty());
    // A bare colon is not a modifier, and neither is a URL's.
    assert_eq!(words(":"), [":"]);
    assert_eq!(words("10:30"), ["10:30"]);
}

/// A quoted phrase is one unit: `"deploy report"` matches the two
/// words together, so highlighting them separately would point at
/// every `report` in the result.
#[test]
fn a_quoted_phrase_is_one_word() {
    assert_eq!(words(r#""deploy report""#), ["deploy report"]);
    assert_eq!(
        words(r#"from:legion "deploy report" now"#),
        ["deploy report", "now"]
    );
    // An unclosed quote is the rest of the query, which is what the
    // server does with one too.
    assert_eq!(words(r#""deploy report"#), ["deploy report"]);
}

/// The point of all of it: the words stand out in the result.
#[test]
fn the_words_come_back_in_bold() {
    assert_eq!(
        highlight("the deploy failed", &["deploy".to_owned()]),
        "the <b>deploy</b> failed"
    );
    // Case-insensitively, and every occurrence.
    assert_eq!(
        highlight("Deploy and deploy", &["deploy".to_owned()]),
        "<b>Deploy</b> and <b>deploy</b>"
    );
    // Two words that overlap are one run of bold rather than nested
    // tags, which Pango would reject.
    assert_eq!(
        highlight("abcd", &["abc".to_owned(), "bcd".to_owned()]),
        "<b>abcd</b>"
    );
    // Nothing to look for is the text, escaped.
    assert_eq!(highlight("a < b", &[]), "a &lt; b");
    // And the escaping still happens around the tags.
    assert_eq!(
        highlight("a <b> c", &["<b>".to_owned()]),
        "a <b>&lt;b&gt;</b> c"
    );
    // A word longer than the text matches nothing and panics on
    // nothing.
    assert_eq!(highlight("ab", &["abcdef".to_owned()]), "ab");
    assert_eq!(highlight("", &["a".to_owned()]), "");
    assert_eq!(highlight("ab", &[String::new()]), "ab");
}

/// Over characters, not bytes: a Cyrillic query against a Cyrillic
/// message would otherwise slice a letter in half and hand Pango
/// invalid UTF-8-shaped markup.
#[test]
fn highlighting_counts_characters_rather_than_bytes() {
    assert_eq!(
        highlight("ошибка в сообщении", &["ошибка".to_owned()]),
        "<b>ошибка</b> в сообщении"
    );
    assert_eq!(highlight("Ошибка", &["ошибка".to_owned()]), "<b>Ошибка</b>");
}

/// Lowercasing can change how many characters there are -- a Turkish
/// dotted capital I becomes a letter and a combining dot -- and an
/// index into the lowered text would then point somewhere else in the
/// original. Rather than mapping between the two, highlighting gives
/// up and draws the text plain: without the bold beats wrong.
#[test]
fn text_that_changes_length_when_lowered_is_drawn_plain() {
    let awkward = "\u{130}stanbul";
    assert_ne!(
        awkward.to_lowercase().chars().count(),
        awkward.chars().count()
    );
    assert_eq!(
        highlight(awkward, &["stanbul".to_owned()]),
        crate::escape(awkward)
    );
}
