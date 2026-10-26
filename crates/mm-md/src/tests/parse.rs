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

#[test]
fn to_plain_renders_links_as_their_label_and_breaks_as_spaces() {
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
