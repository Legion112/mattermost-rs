use super::*;
use crate::parse::parse;

fn render(source: &str) -> String {
    let doc = parse(source);
    match doc.blocks.first() {
        Some(crate::parse::Block::Paragraph(inlines)) => to_pango(inlines),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

#[test]
fn plain_text_passes_through() {
    assert_eq!(render("hello there"), "hello there");
}

#[test]
fn emphasis_strong_and_strike_become_tags() {
    assert_eq!(render("*em*"), "<i>em</i>");
    assert_eq!(render("**strong**"), "<b>strong</b>");
    assert_eq!(render("~~gone~~"), "<s>gone</s>");
    assert_eq!(render("`code`"), "<tt>code</tt>");
}

#[test]
fn nesting_produces_nested_tags() {
    assert_eq!(
        render("**bold *and italic***"),
        "<b>bold <i>and italic</i></b>"
    );
}

#[test]
fn links_render_with_an_escaped_href() {
    assert_eq!(
        render("[text](https://example.invalid/a?b=1&c=2)"),
        "<a href=\"https://example.invalid/a?b=1&amp;c=2\">text</a>"
    );
}

/// The failure mode this guards: GTK does not render malformed markup badly,
/// it logs an error and drops the text. A message containing a bare `<` would
/// vanish entirely.
#[test]
fn the_xml_entities_are_escaped_everywhere() {
    assert_eq!(escape("a & b"), "a &amp; b");
    assert_eq!(escape("<script>"), "&lt;script&gt;");
    assert_eq!(escape(r#"say "hi""#), "say &quot;hi&quot;");
    assert_eq!(escape("it's"), "it&apos;s");
    assert_eq!(escape(""), "");
    assert_eq!(escape("plain"), "plain");
}

#[test]
fn angle_brackets_in_a_message_survive_rendering() {
    let out = render("use <b>tags</b> carefully");
    assert!(
        !out.contains("<b>tags</b>"),
        "must not emit live markup: {out}"
    );
    assert!(out.contains("&lt;b&gt;"), "{out}");
}

#[test]
fn code_spans_escape_their_contents() {
    assert_eq!(render("`a < b && c`"), "<tt>a &lt; b &amp;&amp; c</tt>");
}

/// Mattermost renders a newline inside a paragraph as a line break, not as
/// a space -- a deliberate departure from `CommonMark`. A message typed with
/// shift-enter, and a webhook alert whose summary sits under its severity,
/// read as two lines in every other Mattermost client and ran together in
/// this one.
#[test]
fn soft_breaks_become_line_breaks() {
    assert_eq!(render("one\ntwo"), "one\ntwo");
}

/// The property the plan asks for: for arbitrary input, including input built
/// from the characters most likely to break XML, the markup stays balanced and
/// contains no unescaped entity.
#[test]
fn rendered_markup_is_always_well_formed() {
    let fragments = [
        "a",
        "&",
        "<",
        ">",
        "\"",
        "'",
        "*",
        "**",
        "~~",
        "`",
        "[",
        "]",
        "(",
        ")",
        "\n",
        "  ",
        "</b>",
        "<a href='x'>",
        "&amp;",
        "\\",
        "é",
        "😀",
        "中文",
        "@user",
        "~channel",
    ];

    // A deterministic sweep over combinations rather than a random generator:
    // the interesting inputs here are structural, and a fixed sweep is
    // reproducible when it fails.
    for a in &fragments {
        for b in &fragments {
            for c in &fragments {
                let source = format!("{a}{b}{c}");
                let doc = parse(&source);
                for block in &doc.blocks {
                    let (crate::parse::Block::Paragraph(inlines)
                    | crate::parse::Block::Heading { inlines, .. }
                    | crate::parse::Block::ListItem(inlines)) = block
                    else {
                        continue;
                    };
                    let markup = to_pango(inlines);
                    assert!(
                        is_balanced(&markup),
                        "unbalanced markup for {source:?}: {markup}"
                    );
                    assert!(
                        no_stray_entities(&markup),
                        "unescaped entity for {source:?}: {markup}"
                    );
                }
            }
        }
    }
}

/// Every tag opened is closed, in order.
fn is_balanced(markup: &str) -> bool {
    let mut stack: Vec<String> = Vec::new();
    let mut rest = markup;
    while let Some(at) = rest.find('<') {
        let Some(end) = rest[at..].find('>') else {
            return false;
        };
        let tag = &rest[at + 1..at + end];
        if let Some(name) = tag.strip_prefix('/') {
            let Some(open) = stack.pop() else {
                return false;
            };
            if open != name {
                return false;
            }
        } else {
            // Attributes are part of the opening tag; take the name only.
            let name = tag.split_whitespace().next().unwrap_or(tag);
            stack.push(name.to_owned());
        }
        rest = &rest[at + end + 1..];
    }
    stack.is_empty()
}

/// Outside of tags, no bare `&`, `<` or `>` survives.
fn no_stray_entities(markup: &str) -> bool {
    let mut depth = 0_i32;
    for (at, ch) in markup.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            '&' if depth == 0 => {
                let tail = &markup[at..];
                let known = ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"];
                if !known.iter().any(|e| tail.starts_with(e)) {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

#[test]
fn the_balance_checker_itself_rejects_bad_markup() {
    // A checker that accepts everything would make the property test vacuous.
    assert!(is_balanced("<b>x</b>"));
    assert!(!is_balanced("<b>x"));
    assert!(!is_balanced("x</b>"));
    assert!(!is_balanced("<b>x</i>"));
    assert!(!is_balanced("<b"));
    assert!(is_balanced("<a href=\"x\">y</a>"));

    assert!(no_stray_entities("a &amp; b"));
    assert!(!no_stray_entities("a & b"));
    assert!(no_stray_entities("<b>ok</b>"));
}

#[test]
fn an_empty_document_renders_to_nothing() {
    assert_eq!(to_pango(&[]), "");
}

/// The shape a webhook alert actually arrives in: a severity and a summary
/// separated by one newline. Running them together is what made this
/// client's alert channels read differently from every other one.
#[test]
fn an_alert_body_keeps_its_two_lines() {
    assert_eq!(
        render("**Severity:** Warning\nSummary: it restarts"),
        "<b>Severity:</b> Warning\nSummary: it restarts"
    );
}

/// A mention is drawn in Mattermost's own colours rather than the desktop's
/// accent, so it reads the same here as in every other client the reader
/// has open -- and their own mention louder than everyone else's.
#[test]
fn mentions_carry_their_own_colour() {
    let roster = |name: &str| match name {
        "alukard" => Some(crate::Mentioned::Someone),
        "legion" => Some(crate::Mentioned::Me),
        _ => None,
    };
    let mut doc = crate::parse("@alukard and @legion");
    crate::mark_mentions(&mut doc, &roster);
    let Some(crate::Block::Paragraph(inlines)) = doc.blocks.first() else {
        panic!("expected a paragraph");
    };
    let markup = to_pango(inlines);
    assert!(markup.contains("#5d89ea"), "{markup}");
    assert!(markup.contains("@alukard"), "{markup}");
    // Their own mention gets a background, not just a colour.
    assert!(markup.contains("background="), "{markup}");
    assert!(markup.contains("@legion"), "{markup}");
}
