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

/// A list is a prefix on each line the selection touches, and the
/// selection follows its text: the start moves by the first line's
/// prefix, the end by all of them.
#[test]
fn a_prefix_goes_on_every_touched_line() {
    assert_eq!(
        prefix_lines("one\ntwo\nthree", 1, 9, "- "),
        Wrapped {
            text: "- one\n- two\n- three".to_owned(),
            start: 3,
            end: 15,
        }
    );
}

/// When every touched line has the prefix, pressing again takes it
/// off all of them.
#[test]
fn a_prefix_on_every_line_comes_off() {
    let on = prefix_lines("one\ntwo", 0, 7, "> ");
    let off = prefix_lines(&on.text, on.start, on.end, "> ");
    assert_eq!(off.text, "one\ntwo");
    assert_eq!((off.start, off.end), (0, 7));
}

/// A mixed block -- some lines prefixed, some not -- gets the prefix on
/// the ones missing it rather than losing it from the ones that have it.
#[test]
fn a_mixed_block_is_completed_not_cleared() {
    assert_eq!(prefix_lines("- one\ntwo", 0, 9, "- ").text, "- one\n- two");
}

/// With nothing selected the caret's own line is the one, and the caret
/// moves past the prefix.
#[test]
fn an_empty_selection_prefixes_the_caret_line() {
    assert_eq!(
        prefix_lines("first\nsecond", 8, 8, "### "),
        Wrapped {
            text: "first\n### second".to_owned(),
            start: 12,
            end: 12,
        }
    );
    assert_eq!(prefix_lines("", 0, 0, "- ").text, "- ");
}

/// A selection that ends just after a line break stops at that break:
/// dragging to the start of the next line is not selecting it.
#[test]
fn a_selection_ending_at_a_break_leaves_the_next_line_alone() {
    assert_eq!(prefix_lines("one\ntwo", 0, 4, "- ").text, "- one\ntwo");
}

/// A caret inside the prefix being removed lands where the prefix was,
/// never before the start of its line.
#[test]
fn a_caret_inside_a_removed_prefix_stays_on_its_line() {
    assert_eq!(
        prefix_lines("ab\n- cd", 4, 4, "- "),
        Wrapped {
            text: "ab\ncd".to_owned(),
            start: 3,
            end: 3,
        }
    );
}

/// Numbers count up, and a block already numbered -- however it was
/// numbered -- comes back unnumbered.
#[test]
fn numbering_counts_and_toggles() {
    let on = number_lines("a\nb\nc", 0, 5);
    assert_eq!(on.text, "1. a\n2. b\n3. c");
    let off = number_lines(&on.text, on.start, on.end);
    assert_eq!(off.text, "a\nb\nc");
    assert_eq!((off.start, off.end), (0, 5));
}

/// A line slipped into a numbered list is numbered with the rest, and
/// the rest are renumbered so the list stays in order.
#[test]
fn numbering_a_mixed_block_renumbers_it() {
    assert_eq!(
        number_lines("1. a\nb\n12. c", 0, 12).text,
        "1. a\n2. b\n3. c"
    );
    // Something that only looks numbered is not.
    assert_eq!(number_lines("1.a", 0, 3).text, "1. 1.a");
    assert_eq!(number_lines("12", 0, 2).text, "1. 12");
}

/// A link round the selection, with the placeholder address selected
/// so it can be typed straight over.
#[test]
fn a_selection_becomes_a_link_with_the_address_selected() {
    assert_eq!(
        link("see docs now", 4, 8),
        Wrapped {
            text: "see [docs](url) now".to_owned(),
            start: 11,
            end: 14,
        }
    );
}

/// Nothing selected: a whole placeholder link, with the text selected
/// first since that is what gets written first.
#[test]
fn no_selection_gives_a_placeholder_link() {
    assert_eq!(
        link("say ", 4, 4),
        Wrapped {
            text: "say [text](url)".to_owned(),
            start: 5,
            end: 9,
        }
    );
    // Offsets past the end are the end, backwards offsets are put the
    // right way round, as with `wrap`.
    assert_eq!(link("ab", 9, 1).text, "a[b](url)");
}

/// A URL pasted over a selection links the selection to it, with the
/// caret after; over nothing it is just text, as any paste is.
#[test]
fn a_pasted_address_links_the_selection() {
    assert_eq!(
        paste_link("see docs now", 4, 8, "https://x.test/d"),
        Wrapped {
            text: "see [docs](https://x.test/d) now".to_owned(),
            start: 28,
            end: 28,
        }
    );
    assert_eq!(
        paste_link("see ", 4, 4, "https://x.test"),
        Wrapped {
            text: "see https://x.test".to_owned(),
            start: 18,
            end: 18,
        }
    );
    // Backwards and out-of-range offsets, as with `wrap`.
    assert_eq!(paste_link("ab", 9, 1, "http://h").text, "a[b](http://h)");
}

/// Only one bare address counts: a scheme, nothing after it that is not
/// part of it, and no second word.
#[test]
fn only_a_bare_address_is_one() {
    assert!(is_web_address("https://example.test/a?b=c"));
    assert!(is_web_address("  http://h/ \n"));
    assert!(!is_web_address("https://"));
    assert!(!is_web_address("see https://h"));
    assert!(!is_web_address("https://h and more"));
    assert!(!is_web_address("ftp://h"));
}
