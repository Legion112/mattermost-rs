use super::*;

/// The ordinary case, and the caret afterwards: whatever was selected
/// stays selected, so pressing bold then italic nests them.
#[test]
fn a_selection_gets_the_markers_round_it() {
    assert_eq!(
        wrap("make this bold", 10, 14, "**"),
        Wrapped {
            text: "make this **bold**".to_owned(),
            start: 12,
            end: 16,
        }
    );
}

/// Nothing selected puts the markers in and leaves the caret between
/// them, ready to type.
#[test]
fn an_empty_selection_leaves_the_caret_between_the_markers() {
    assert_eq!(
        wrap("say ", 4, 4, "**"),
        Wrapped {
            text: "say ****".to_owned(),
            start: 6,
            end: 6,
        }
    );
}

/// A toggle: pressing bold twice leaves what you started with. The
/// markers are looked for outside the selection, which is what
/// double-clicking the word inside `**word**` gives you.
#[test]
fn pressing_it_again_takes_the_markers_off() {
    let on = wrap("make this bold", 10, 14, "**");
    let off = wrap(&on.text, on.start, on.end, "**");
    assert_eq!(off.text, "make this bold");
    assert_eq!((off.start, off.end), (10, 14));

    // And when the selection includes the markers, which is what
    // dragging across them gives.
    assert_eq!(
        wrap("a **b** c", 2, 7, "**"),
        Wrapped {
            text: "a b c".to_owned(),
            start: 2,
            end: 3,
        }
    );
}

/// Markers of one character work the same way, and so does a
/// selection at either end of the text.
#[test]
fn the_marker_may_be_one_character_and_the_selection_may_touch_the_ends() {
    assert_eq!(wrap("word", 0, 4, "*").text, "*word*");
    assert_eq!(wrap("*word*", 1, 5, "*").text, "word");
    assert_eq!(wrap("", 0, 0, "`").text, "``");
}

/// Offsets are characters, not bytes: a Cyrillic message would
/// otherwise be sliced in the middle of a letter.
#[test]
fn offsets_count_characters() {
    assert_eq!(
        wrap("ошибка в сообщении", 0, 6, "**"),
        Wrapped {
            text: "**ошибка** в сообщении".to_owned(),
            start: 2,
            end: 8,
        }
    );
}

/// Total over nonsense: offsets past the end, and the wrong way
/// round, are clamped rather than panicking on a buffer that moved
/// between the key press and the handler.
#[test]
fn offsets_out_of_range_are_clamped() {
    assert_eq!(wrap("ab", 9, 9, "*").text, "ab**");
    assert_eq!(wrap("ab", 2, 0, "*").text, "*ab*");
    // Every derive is exercised.
    let one = wrap("a", 0, 1, "*");
    assert_eq!(one, one.clone());
    assert!(!format!("{one:?}").is_empty());
}
