use super::*;

fn para(doc: &Document) -> &[Inline] {
    match doc.blocks.first() {
        Some(Block::Paragraph(inlines)) => inlines,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

#[test]
fn plain_text_is_one_paragraph_of_one_text_inline() {
    let doc = parse("hello there");
    assert_eq!(para(&doc), &[Inline::Text("hello there".into())]);
    assert_eq!(doc.plain_text(), Some("hello there"));
}

/// Most messages are exactly this, and the GUI uses it to skip building a
/// markup string it would only have to parse back.
#[test]
fn plain_text_recognises_only_the_simple_case() {
    assert!(parse("**bold**").plain_text().is_none());
    assert!(parse("a\n\nb").plain_text().is_none());
    assert!(parse("# heading").plain_text().is_none());
    assert!(parse("").plain_text().is_none());
}

#[test]
fn emphasis_strong_strike_and_code_parse() {
    assert_eq!(
        para(&parse("*em*")),
        &[Inline::Emphasis(vec![Inline::Text("em".into())])]
    );
    assert_eq!(
        para(&parse("**strong**")),
        &[Inline::Strong(vec![Inline::Text("strong".into())])]
    );
    assert_eq!(
        para(&parse("~~gone~~")),
        &[Inline::Strike(vec![Inline::Text("gone".into())])]
    );
    assert_eq!(para(&parse("`x`")), &[Inline::Code("x".into())]);
}

#[test]
fn nested_emphasis_nests() {
    assert_eq!(
        para(&parse("**a *b***")),
        &[Inline::Strong(vec![
            Inline::Text("a ".into()),
            Inline::Emphasis(vec![Inline::Text("b".into())]),
        ])]
    );
}

#[test]
fn links_carry_their_destination() {
    assert_eq!(
        para(&parse("[text](https://example.invalid)")),
        &[Inline::Link {
            href: "https://example.invalid".into(),
            text: "text".into(),
        }]
    );
}

/// A bare autolink has no label, and showing an empty link would lose the URL
/// entirely.
#[test]
fn a_link_with_no_label_falls_back_to_its_url() {
    let doc = parse("<https://example.invalid>");
    assert_eq!(
        para(&doc),
        &[Inline::Link {
            href: "https://example.invalid".into(),
            text: "https://example.invalid".into(),
        }]
    );
}

#[test]
fn fenced_code_keeps_its_language_and_body() {
    let doc = parse("```rust\nfn main() {}\n```");
    assert_eq!(
        doc.blocks,
        vec![Block::Code {
            lang: Some("rust".into()),
            code: "fn main() {}\n".into(),
        }]
    );
}

#[test]
fn a_fence_without_a_language_has_none() {
    let doc = parse("```\nplain\n```");
    assert_eq!(
        doc.blocks,
        vec![Block::Code {
            lang: None,
            code: "plain\n".into(),
        }]
    );
}

#[test]
fn indented_code_is_a_code_block_too() {
    let doc = parse("    indented\n");
    assert_eq!(
        doc.blocks,
        vec![Block::Code {
            lang: None,
            code: "indented\n".into(),
        }]
    );
}

#[test]
fn headings_carry_their_level() {
    for (source, level) in [("# a", 1), ("## a", 2), ("### a", 3)] {
        let doc = parse(source);
        assert_eq!(
            doc.blocks,
            vec![Block::Heading {
                level,
                inlines: vec![Inline::Text("a".into())],
            }],
            "{source}"
        );
    }
}

#[test]
fn quotes_contain_their_own_blocks() {
    let doc = parse("> quoted");
    assert_eq!(
        doc.blocks,
        vec![Block::Quote(vec![Block::Paragraph(vec![Inline::Text(
            "quoted".into()
        )])])]
    );
}

#[test]
fn list_items_become_flat_items() {
    let doc = parse("- one\n- two");
    assert_eq!(
        doc.blocks,
        vec![
            Block::ListItem(vec![Inline::Text("one".into())]),
            Block::ListItem(vec![Inline::Text("two".into())]),
        ]
    );
}

#[test]
fn a_rule_is_a_block() {
    assert_eq!(parse("---").blocks, vec![Block::Rule]);
}

#[test]
fn a_soft_break_inside_a_paragraph_is_preserved() {
    assert_eq!(
        para(&parse("one\ntwo")),
        &[
            Inline::Text("one".into()),
            Inline::SoftBreak,
            Inline::Text("two".into()),
        ]
    );
}

/// A message need not end in a blank line, and dropping the final paragraph
/// would silently lose the last thing somebody typed.
#[test]
fn a_message_with_no_trailing_newline_keeps_its_last_paragraph() {
    let doc = parse("last line with no newline");
    assert_eq!(doc.blocks.len(), 1);
}

/// Tables are not enabled, so the source stays readable rather than being
/// mangled into a paragraph of pipes with the structure lost.
#[test]
fn unsupported_syntax_degrades_to_readable_text() {
    let doc = parse("| a | b |\n|---|---|\n| 1 | 2 |");
    assert!(!doc.to_plain().is_empty());
    assert!(doc.to_plain().contains('a'));
}

#[test]
fn to_plain_strips_markup_across_every_block_kind() {
    let doc =
        parse("# Title\n\n**bold** and `code`\n\n> quoted\n\n- item\n\n---\n\n```\nfn x\n```");
    let plain = doc.to_plain();
    assert!(plain.contains("Title"), "{plain}");
    assert!(plain.contains("bold and code"), "{plain}");
    assert!(plain.contains("quoted"), "{plain}");
    assert!(plain.contains("• item"), "{plain}");
    assert!(plain.contains("---"), "{plain}");
    assert!(plain.contains("fn x"), "{plain}");
    assert!(!plain.contains("**"), "{plain}");
}

/// `to_plain` is the one-line summary a notification body and a search
/// index want, so it collapses a break to a space -- unlike the display
/// path, which keeps it as a line break the way Mattermost does.
#[test]
fn to_plain_renders_links_as_their_label_and_collapses_breaks() {
    assert_eq!(parse("[label](https://x.invalid)").to_plain(), "label");
    assert_eq!(parse("a\nb").to_plain(), "a b");
    assert_eq!(
        parse("*em* **strong** ~~gone~~").to_plain(),
        "em strong gone"
    );
}

#[test]
fn an_empty_message_parses_to_an_empty_document() {
    let doc = parse("");
    assert!(doc.blocks.is_empty());
    assert_eq!(doc.to_plain(), "");
    assert_eq!(Document::default().to_plain(), "");
}

/// Parsing must never panic, whatever arrives from the server.
#[test]
fn hostile_input_does_not_panic() {
    for source in [
        "***",
        "[](",
        "```",
        "> > > deep",
        "- \n- \n- ",
        &"*".repeat(500),
        &"> ".repeat(200),
        "\u{0}\u{1}",
        "😀🏳️‍🌈中文",
        "<script>alert(1)</script>",
    ] {
        let doc = parse(source);
        let _ = doc.to_plain();
        let _ = doc.plain_text();
    }
}

#[test]
fn every_derive_is_exercised() {
    let doc = parse("**a**");
    assert_eq!(doc, doc.clone());
    assert!(!format!("{doc:?}").is_empty());

    let block = Block::Rule;
    assert_eq!(block, block.clone());
    assert!(!format!("{block:?}").is_empty());
    assert_ne!(Block::Rule, Block::Paragraph(Vec::new()));

    let inline = Inline::SoftBreak;
    assert_eq!(inline, inline.clone());
    assert!(!format!("{inline:?}").is_empty());
    assert_ne!(Inline::SoftBreak, Inline::Text(String::new()));
}

/// Raw HTML must survive as literal text. Dropping it would silently change
/// what somebody typed, and interpreting it would let a message inject markup
/// into the renderer.
#[test]
fn raw_html_is_kept_as_text_not_interpreted() {
    let doc = parse("use <b>tags</b> carefully");
    let plain = doc.to_plain();
    assert!(plain.contains("<b>"), "{plain}");
    assert!(plain.contains("</b>"), "{plain}");

    let block = parse("<div>\nstuff\n</div>");
    assert!(block.to_plain().contains("<div>"), "{:?}", block.to_plain());
}

/// Inside a fence, HTML is part of the code and must not become an inline.
#[test]
fn html_inside_a_code_fence_stays_in_the_code() {
    let doc = parse("```html\n<b>x</b>\n```");
    assert_eq!(
        doc.blocks,
        vec![Block::Code {
            lang: Some("html".into()),
            code: "<b>x</b>\n".into(),
        }]
    );
}

// The builder is defensive about event sequences pulldown-cmark does not
// currently produce -- a closing tag with nothing open, an unclosed quote at
// end of input. That defensiveness is worth keeping, because a parser upgrade
// can reorder events, but it is only worth keeping if it is known to behave
// sanely. These drive the builder directly to prove it does.

use pulldown_cmark::CowStr;

#[test]
fn closing_tags_with_nothing_open_are_ignored() {
    let mut builder = Builder::default();
    builder.on_event(&Event::End(TagEnd::Emphasis));
    builder.on_event(&Event::End(TagEnd::Link));
    builder.on_event(&Event::End(TagEnd::CodeBlock));
    builder.on_event(&Event::End(TagEnd::List(false)));
    let doc = builder.finish();
    assert!(doc.blocks.is_empty(), "{doc:?}");
}

#[test]
fn leaf_events_the_renderer_does_not_draw_are_dropped() {
    let mut builder = Builder::default();
    builder.on_event(&Event::FootnoteReference(CowStr::Borrowed("1")));
    builder.on_event(&Event::TaskListMarker(true));
    let doc = builder.finish();
    assert!(doc.blocks.is_empty(), "{doc:?}");
}

#[test]
fn start_tags_with_no_handling_do_not_disturb_the_surrounding_text() {
    let mut builder = Builder::default();
    builder.on_event(&Event::Start(Tag::List(None)));
    builder.on_event(&Event::Text(CowStr::Borrowed("text")));
    builder.on_event(&Event::End(TagEnd::Paragraph));
    assert_eq!(
        builder.finish().blocks,
        vec![Block::Paragraph(vec![Inline::Text("text".into())])]
    );
}

/// An unclosed quote at end of input must still yield its contents, not drop
/// them.
#[test]
fn an_unclosed_quote_still_produces_its_blocks() {
    let mut builder = Builder::default();
    builder.on_event(&Event::Start(Tag::BlockQuote(None)));
    builder.on_event(&Event::Text(CowStr::Borrowed("inside")));
    builder.on_event(&Event::End(TagEnd::Paragraph));
    assert_eq!(
        builder.finish().blocks,
        vec![Block::Quote(vec![Block::Paragraph(vec![Inline::Text(
            "inside".into()
        )])])]
    );
}

/// If HTML ever arrives while a code block is open, it belongs to the code
/// rather than becoming an inline alongside it.
#[test]
fn html_arriving_inside_a_code_block_joins_the_code() {
    let mut builder = Builder::default();
    builder.on_event(&Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(
        CowStr::Borrowed(""),
    ))));
    builder.on_event(&Event::Html(CowStr::Borrowed("<b>raw</b>")));
    builder.on_event(&Event::End(TagEnd::CodeBlock));
    assert_eq!(
        builder.finish().blocks,
        vec![Block::Code {
            lang: None,
            code: "<b>raw</b>".into(),
        }]
    );
}

// Mattermost linkifies bare URLs; CommonMark only linkifies <wrapped> ones.
// Without this pass every link in every channel is dead text.

fn inlines_of(source: &str) -> Vec<Inline> {
    match parse(source).blocks.into_iter().next() {
        Some(Block::Paragraph(inlines)) => inlines,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

fn link(url: &str) -> Inline {
    Inline::Link {
        href: url.into(),
        text: url.into(),
    }
}

#[test]
fn a_bare_url_becomes_a_link() {
    assert_eq!(
        inlines_of("https://example.invalid/a"),
        vec![link("https://example.invalid/a")]
    );
    assert_eq!(
        inlines_of("http://example.invalid"),
        vec![link("http://example.invalid")]
    );
}

#[test]
fn text_around_a_url_is_preserved() {
    assert_eq!(
        inlines_of("see https://example.invalid/a now"),
        vec![
            Inline::Text("see ".into()),
            link("https://example.invalid/a"),
            Inline::Text(" now".into()),
        ]
    );
}

#[test]
fn several_urls_in_one_line_all_become_links() {
    let parsed = inlines_of("https://a.invalid and https://b.invalid");
    assert_eq!(
        parsed
            .iter()
            .filter(|i| matches!(i, Inline::Link { .. }))
            .count(),
        2
    );
}

/// A URL at the end of a sentence is followed by prose punctuation, which is
/// not part of the address.
#[test]
fn sentence_punctuation_is_not_swallowed_into_the_url() {
    for (source, url, tail) in [
        (
            "see https://example.invalid.",
            "https://example.invalid",
            ".",
        ),
        (
            "see https://example.invalid, then",
            "https://example.invalid",
            ", then",
        ),
        (
            "see https://example.invalid!",
            "https://example.invalid",
            "!",
        ),
        (
            "see https://example.invalid?",
            "https://example.invalid",
            "?",
        ),
    ] {
        let parsed = inlines_of(source);
        assert_eq!(parsed[1], link(url), "{source}");
        assert_eq!(parsed[2], Inline::Text(tail.into()), "{source}");
    }
}

/// A closing bracket is kept only when the URL opened one itself, which is
/// what makes Wikipedia-style links survive while "(see <https://x>)" does not
/// eat the bracket.
#[test]
fn brackets_are_kept_only_when_the_url_opened_them() {
    let balanced = inlines_of("https://example.invalid/a_(b)");
    assert_eq!(balanced[0], link("https://example.invalid/a_(b)"));

    let prose = inlines_of("(see https://example.invalid)");
    assert_eq!(prose[1], link("https://example.invalid"));
    assert_eq!(prose[2], Inline::Text(")".into()));

    let square = inlines_of("https://example.invalid/a]");
    assert_eq!(square[0], link("https://example.invalid/a"));
}

/// Starting mid-token is not a URL.
#[test]
fn a_scheme_inside_a_word_is_not_linked() {
    let parsed = inlines_of("nothttps://example.invalid");
    assert!(
        parsed.iter().all(|i| !matches!(i, Inline::Link { .. })),
        "{parsed:?}"
    );
}

#[test]
fn a_scheme_with_nothing_after_it_is_not_a_link() {
    for source in ["https://", "http://", "see https:// here"] {
        let parsed = inlines_of(source);
        assert!(
            parsed.iter().all(|i| !matches!(i, Inline::Link { .. })),
            "{source}: {parsed:?}"
        );
    }
}

/// An explicit markdown link must keep its own label rather than being
/// relabelled with its URL.
#[test]
fn an_explicit_link_is_left_alone() {
    assert_eq!(
        inlines_of("[label](https://example.invalid)"),
        vec![Inline::Link {
            href: "https://example.invalid".into(),
            text: "label".into(),
        }]
    );
}

/// A URL in code is text, not a link.
#[test]
fn urls_in_code_are_not_linked() {
    assert_eq!(
        inlines_of("`https://example.invalid`"),
        vec![Inline::Code("https://example.invalid".into())]
    );
    assert_eq!(
        parse("```\nhttps://example.invalid\n```").blocks,
        vec![Block::Code {
            lang: None,
            code: "https://example.invalid\n".into(),
        }]
    );
}

#[test]
fn urls_are_linked_inside_emphasis_and_other_blocks() {
    let bold = inlines_of("**https://example.invalid**");
    assert_eq!(
        bold,
        vec![Inline::Strong(vec![link("https://example.invalid")])]
    );

    let quoted = parse("> https://example.invalid");
    assert_eq!(
        quoted.blocks,
        vec![Block::Quote(vec![Block::Paragraph(vec![link(
            "https://example.invalid"
        )])])]
    );

    let item = parse("- https://example.invalid");
    assert_eq!(
        item.blocks,
        vec![Block::ListItem(vec![link("https://example.invalid")])]
    );

    let heading = parse("# https://example.invalid");
    assert_eq!(
        heading.blocks,
        vec![Block::Heading {
            level: 1,
            inlines: vec![link("https://example.invalid")],
        }]
    );

    assert_eq!(parse("---").blocks, vec![Block::Rule]);
}

#[test]
fn text_with_no_url_is_untouched() {
    assert_eq!(
        inlines_of("just some words"),
        vec![Inline::Text("just some words".into())]
    );
}

/// Linkifying must not panic on anything, and must not split a character.
#[test]
fn autolinking_hostile_input_does_not_panic() {
    for source in [
        "https://пример.рф/путь",
        "https://例え.テスト",
        "https://a.invalid/😀",
        "http://",
        "https://)))",
        "...https://a.invalid...",
        &format!("https://a.invalid/{}", "x".repeat(2000)),
    ] {
        let doc = parse(source);
        let _ = doc.to_plain();
    }
}

/// pulldown-cmark splits a text run at every character that might have been
/// markup, so a single typed word can arrive as several Text events. Anything
/// that inspects a text inline as a whole has to see it whole.
#[test]
fn adjacent_text_runs_are_merged_into_one_inline() {
    // `_` and `]` are both split points for the parser.
    assert_eq!(
        inlines_of("plain_text_with_underscores"),
        vec![Inline::Text("plain_text_with_underscores".into())]
    );
    assert_eq!(inlines_of("a]b"), vec![Inline::Text("a]b".into())]);
}

/// The reason the merge matters: without it the autolinker sees only the
/// fragment before the first split point and truncates the address.
#[test]
fn a_url_containing_a_split_point_is_linked_whole() {
    assert_eq!(
        inlines_of("https://example.invalid/a_(b)"),
        vec![link("https://example.invalid/a_(b)")]
    );
    assert_eq!(
        inlines_of("https://example.invalid/path_with_underscores"),
        vec![link("https://example.invalid/path_with_underscores")]
    );
}

/// A soft break is a real inline, so it must still separate the runs either
/// side of it rather than being swallowed by the merge.
#[test]
fn merging_does_not_cross_a_soft_break() {
    assert_eq!(
        inlines_of("one\ntwo"),
        vec![
            Inline::Text("one".into()),
            Inline::SoftBreak,
            Inline::Text("two".into()),
        ]
    );
}

/// Exercised directly because a URL always begins `http`, so the public path
/// can never trim one away entirely — and a loop that can empty its input
/// should be known to terminate rather than assumed to.
#[test]
fn trimming_a_string_that_is_all_punctuation_leaves_nothing() {
    assert_eq!(trim_url_tail("..."), "");
    assert_eq!(trim_url_tail(""), "");
    assert_eq!(trim_url_tail("https://a.invalid"), "https://a.invalid");
    assert_eq!(trim_url_tail("https://a.invalid)."), "https://a.invalid");
    assert_eq!(
        trim_url_tail("https://a.invalid/(x)"),
        "https://a.invalid/(x)"
    );
}

/// Emoji shortcodes are translated in ordinary message text.
#[test]
fn shortcodes_in_a_paragraph_become_glyphs() {
    let doc = parse("ship it :tada:");
    assert_eq!(doc.plain_text(), Some("ship it 🎉"));
}

/// A shortcode inside code is text, exactly like a URL inside code is text.
#[test]
fn shortcodes_in_code_are_left_alone() {
    let doc = parse("`:tada:`");
    assert_eq!(doc.to_plain(), ":tada:");
    let fenced = parse("```\n:tada:\n```");
    assert_eq!(fenced.to_plain(), ":tada:");
}

/// A link's URL is decided before emoji are, so a colon-shaped fragment in
/// a URL cannot be rewritten out from under it.
#[test]
fn a_url_is_not_rewritten_by_the_emoji_pass() {
    let doc = parse("see https://example.com/a:tada:b now");
    assert_eq!(doc.to_plain(), "see https://example.com/a:tada:b now");
}
