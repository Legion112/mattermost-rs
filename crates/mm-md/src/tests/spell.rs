use super::*;

fn found(text: &str) -> Vec<&str> {
    // Leaked rather than cloned: the assertions read better against
    // `&str`, and a test process ending is the whole lifetime anyway.
    words(text)
        .into_iter()
        .map(|w| Box::leak(w.text.into_boxed_str()) as &str)
        .collect()
}

/// Plain prose in either script is all words, and the offsets point at
/// the text they came from -- in characters, because a GTK text buffer
/// counts in characters and this sentence is Cyrillic.
#[test]
fn prose_is_words_and_the_offsets_are_characters() {
    let text = "Ошибки в сообщениии";
    assert_eq!(found(text), ["Ошибки", "сообщениии"]);
    let chars: Vec<char> = text.chars().collect();
    for word in words(text) {
        let at: String = chars[word.start..word.end].iter().collect();
        assert_eq!(at, word.text);
    }
    assert_eq!(
        words("Hello there"),
        [
            Word {
                start: 0,
                end: 5,
                text: "Hello".into()
            },
            Word {
                start: 6,
                end: 11,
                text: "there".into()
            },
        ]
    );
}

/// None of this is prose, and underlining it would make a composer in a
/// channel like this one solid red.
#[test]
fn identifiers_mentions_links_and_emoji_are_left_alone() {
    for text in [
        "@legion",
        "~town-square",
        ":tada:",
        "https://jira.webtime.tech/browse/PP-15128",
        "www.example.com",
        "postback_gate",
        "PP-14869",
        "stage-1.91.19",
        "/var/www/alanbase",
        "<https://example.com>",
        "42",
    ] {
        assert!(words(text).is_empty(), "{text} should not be checked");
    }
    // The hyphen is only innocent when there is no digit in the token:
    // "PP-14869" is one thing, "well-known" is two words.
    assert_eq!(found("well-known"), ["well", "known"]);
}

/// Backticks mean the author is quoting something the dictionary has
/// never heard of. An opener with no closer runs to the end, which is
/// what a half-typed fence is.
#[test]
fn code_is_not_prose() {
    assert_eq!(found("say `teh wrng` here"), ["say", "here"]);
    assert_eq!(found("```\nteh wrng\n```\nafter"), ["after"]);
    assert_eq!(found("open ``` teh wrng"), ["open"]);
    assert_eq!(found("open `teh wrng"), ["open"]);
    // Backticks inside a fence are content, not a second fence.
    assert_eq!(found("```\na ` b\n```\nafter"), ["after"]);
    // And an inline span reopens afterwards.
    assert_eq!(found("`a` mid `b` end"), ["mid", "end"]);
}

/// An apostrophe belongs inside a contraction and not around a quote,
/// a single letter is never a misspelling worth drawing, and an acronym
/// is spelled the way its author meant however little the dictionary
/// knows about it.
#[test]
fn contractions_single_letters_and_acronyms() {
    assert_eq!(found("don't"), ["don't"]);
    assert_eq!(found("'quoted' word"), ["quoted", "word"]);
    assert_eq!(found("a я x"), Vec::<&str>::new());
    assert_eq!(found("API and PHP are fine"), ["and", "are", "fine"]);
    // Mixed case is a word again: "Api" is not an acronym.
    assert_eq!(found("Api"), ["Api"]);
    assert!(words("").is_empty());
    // Punctuation around prose comes off.
    assert_eq!(found("Привет, как дела?"), ["Привет", "как", "дела"]);
}

/// Every derive is exercised, which the gate insists on.
#[test]
fn every_derive_is_exercised() {
    let word = words("hello")[0].clone();
    assert_eq!(word, word.clone());
    assert!(!format!("{word:?}").is_empty());
}
