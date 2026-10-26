//! Markdown to an abstract document.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// A parsed message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    /// The common case: a single paragraph of plain text, with no markup at
    /// all.
    ///
    /// Worth asking, because it is most messages, and it lets the GUI emit a
    /// plain `Label` instead of building a markup string and parsing it back.
    /// That is where most of the rendering cost goes in a busy channel.
    #[must_use]
    pub fn plain_text(&self) -> Option<&str> {
        match self.blocks.as_slice() {
            [Block::Paragraph(inlines)] => match inlines.as_slice() {
                [Inline::Text(text)] => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    /// Everything the document says, with markup removed. Used for
    /// notification bodies and for search.
    #[must_use]
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            if !out.is_empty() {
                out.push('\n');
            }
            match block {
                Block::Paragraph(inlines) | Block::Heading { inlines, .. } => {
                    push_plain(&mut out, inlines);
                }
                Block::Quote(blocks) => {
                    let inner = Document {
                        blocks: blocks.clone(),
                    };
                    out.push_str(&inner.to_plain());
                }
                Block::Code { code, .. } => out.push_str(code.trim_end()),
                Block::ListItem(inlines) => {
                    out.push_str("• ");
                    push_plain(&mut out, inlines);
                }
                Block::Rule => out.push_str("---"),
            }
        }
        out
    }
}

fn push_plain(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Mention { name, .. } => {
                out.push('@');
                out.push_str(name);
            }
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                push_plain(out, inner);
            }
            Inline::Link { text, .. } => out.push_str(text),
            // A space, not a newline, unlike the display path: this is the
            // one-line summary a notification body and a search index want.
            Inline::SoftBreak => out.push(' '),
        }
    }
}

/// A block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    /// A fenced or indented code block. `lang` is the fence info string.
    Code {
        lang: Option<String>,
        code: String,
    },
    Quote(Vec<Block>),
    /// Flattened: nesting is dropped, which is honest about what the renderer
    /// currently does rather than carrying structure nothing reads.
    ListItem(Vec<Inline>),
    Rule,
}

/// An inline element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link {
        href: String,
        text: String,
    },
    /// An `@mention`. `is_me` marks the reader's own, which Mattermost
    /// highlights more loudly than everyone else's.
    Mention {
        name: String,
        is_me: bool,
    },
    /// A newline inside a paragraph.
    ///
    /// **Mattermost renders this as a line break, not as a space.** That is
    /// a deliberate departure from `CommonMark`, and it is what every other
    /// Mattermost client does: a message typed with shift-enter, and an
    /// alert whose summary sits under its severity, both read as two lines
    /// everywhere else and ran together here.
    SoftBreak,
}

/// Parse a message.
///
/// Tables, footnotes and task lists are deliberately not enabled: they are not
/// rendered yet, and enabling a feature the renderer ignores turns a table
/// into a mangled paragraph instead of leaving it as readable source.
#[must_use]
pub fn parse(source: &str) -> Document {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.on_event(&event);
    }
    builder.finish()
}

enum Emphasis {
    Em,
    Strong,
    Strike,
}

impl Emphasis {
    fn wrap(self, inner: Vec<Inline>) -> Inline {
        match self {
            Self::Em => Inline::Emphasis(inner),
            Self::Strong => Inline::Strong(inner),
            Self::Strike => Inline::Strike(inner),
        }
    }
}

/// Accumulates events into blocks.
///
/// A struct rather than a pile of locals so the event handling can be split
/// into readable pieces; the state is genuinely shared across all three.
#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    /// The blocks of an open quote, if any.
    quote: Option<Vec<Block>>,
    inlines: Vec<Inline>,
    emphasis: Vec<(Emphasis, Vec<Inline>)>,
    code: Option<(Option<String>, String)>,
    link: Option<(String, String)>,
    heading: Option<u8>,
}

impl Builder {
    fn on_event(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(tag) => self.on_start(tag),
            Event::End(tag) => self.on_end(*tag),
            _ => self.on_leaf(event),
        }
    }

    fn on_start(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Emphasis => self.emphasis.push((Emphasis::Em, Vec::new())),
            Tag::Strong => self.emphasis.push((Emphasis::Strong, Vec::new())),
            Tag::Strikethrough => self.emphasis.push((Emphasis::Strike, Vec::new())),
            Tag::Link { dest_url, .. } => {
                self.link = Some((dest_url.to_string(), String::new()));
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) if !info.is_empty() => Some(info.to_string()),
                    CodeBlockKind::Fenced(_) | CodeBlockKind::Indented => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::BlockQuote(_) => self.quote = Some(Vec::new()),
            Tag::Heading { level, .. } => self.heading = Some(*level as u8),
            _ => {}
        }
    }

    fn on_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                if let Some((kind, inner)) = self.emphasis.pop() {
                    self.push_inline(kind.wrap(inner));
                }
            }
            TagEnd::Link => {
                if let Some((href, text)) = self.link.take() {
                    // A bare autolink has no label, and an empty link would
                    // lose the URL entirely.
                    let text = if text.is_empty() { href.clone() } else { text };
                    self.push_inline(Inline::Link { href, text });
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code.take() {
                    self.target().push(Block::Code { lang, code });
                }
            }
            TagEnd::BlockQuote(_) => {
                if let Some(inner) = self.quote.take() {
                    self.blocks.push(Block::Quote(inner));
                }
            }
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                self.flush(|inlines| Block::Heading { level, inlines });
            }
            TagEnd::Item => self.flush(Block::ListItem),
            TagEnd::Paragraph => self.flush(Block::Paragraph),
            _ => {}
        }
    }

    fn on_leaf(&mut self, event: &Event<'_>) {
        match event {
            Event::Text(text) => {
                if let Some((_, body)) = self.code.as_mut() {
                    body.push_str(text);
                } else if let Some((_, label)) = self.link.as_mut() {
                    label.push_str(text);
                } else {
                    self.push_inline(Inline::Text(text.to_string()));
                }
            }
            Event::Code(text) => self.push_inline(Inline::Code(text.to_string())),
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, body)) = self.code.as_mut() {
                    body.push('\n');
                } else {
                    self.push_inline(Inline::SoftBreak);
                }
            }
            // Raw HTML is kept as literal text, never interpreted. Mattermost
            // does not render HTML in messages either, and dropping it would
            // silently alter what somebody typed: `use <b>tags</b> carefully`
            // would arrive as "use tags carefully". The renderer escapes it,
            // so the reader sees the characters that were sent.
            Event::Html(raw) | Event::InlineHtml(raw) => {
                if let Some((_, body)) = self.code.as_mut() {
                    body.push_str(raw);
                } else {
                    self.push_inline(Inline::Text(raw.to_string()));
                }
            }
            Event::Rule => self.target().push(Block::Rule),
            // Footnotes, tables and task markers are not enabled; dropping
            // their markers leaves the surrounding text intact, which beats
            // emitting something nothing draws.
            _ => {}
        }
    }

    /// Close the open run of inlines into a block, if there is one.
    fn flush(&mut self, into: impl FnOnce(Vec<Inline>) -> Block) {
        let taken = std::mem::take(&mut self.inlines);
        if !taken.is_empty() {
            self.target().push(into(taken));
        }
    }

    /// Append an inline to whichever emphasis span is currently open,
    /// merging it into the previous one when both are plain text.
    ///
    /// The merge is not an optimisation. pulldown-cmark splits a run of text
    /// at characters that *might* have been markup, so
    /// `https://example.invalid/a_(b)` arrives as three separate Text events:
    /// `…/a`, `_`, `(b)`. Keeping them separate means anything that looks at a
    /// text inline as a whole — the autolinker, most obviously — sees only the
    /// first fragment and silently truncates the URL.
    fn push_inline(&mut self, inline: Inline) {
        let target = if let Some((_, inner)) = self.emphasis.last_mut() {
            inner
        } else {
            &mut self.inlines
        };
        if let Inline::Text(addition) = &inline
            && let Some(Inline::Text(existing)) = target.last_mut()
        {
            existing.push_str(addition);
            return;
        }
        target.push(inline);
    }

    /// Blocks go into the open quote if there is one.
    fn target(&mut self) -> &mut Vec<Block> {
        match &mut self.quote {
            Some(inner) => inner,
            None => &mut self.blocks,
        }
    }

    fn finish(mut self) -> Document {
        // A message need not end with a blank line, so whatever is still open
        // is a final paragraph. Dropping it would lose the last thing somebody
        // typed.
        self.flush(Block::Paragraph);
        if let Some(inner) = self.quote.take() {
            self.blocks.push(Block::Quote(inner));
        }
        Document {
            blocks: self.blocks.into_iter().map(linkify_block).collect(),
        }
    }
}

/// Turn bare URLs in text into links.
///
/// `CommonMark` only autolinks `<wrapped>` URLs, but Mattermost linkifies plain
/// ones — and in practice almost every URL anybody posts is plain. Without
/// this, every link in every channel is dead text.
///
/// Applied as a pass over the finished document rather than during parsing so
/// it cannot reach inside a code block or relabel an explicit `[text](url)`.
fn linkify_block(block: Block) -> Block {
    match block {
        Block::Paragraph(inlines) => Block::Paragraph(linkify_inlines(inlines)),
        Block::Heading { level, inlines } => Block::Heading {
            level,
            inlines: linkify_inlines(inlines),
        },
        Block::ListItem(inlines) => Block::ListItem(linkify_inlines(inlines)),
        Block::Quote(blocks) => Block::Quote(blocks.into_iter().map(linkify_block).collect()),
        // Code is left exactly as typed; a URL in a code block is text.
        other @ (Block::Code { .. } | Block::Rule) => other,
    }
}

fn linkify_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    inlines
        .into_iter()
        .flat_map(|inline| match inline {
            // Emoji after linkifying, not before: a URL may legitimately
            // contain something shaped like a shortcode, and once autolink
            // has decided what is a link, only the leftover text is rewritten.
            Inline::Text(text) => autolink(&text)
                .into_iter()
                .map(|piece| match piece {
                    Inline::Text(t) => Inline::Text(crate::emoji::substitute(&t)),
                    other => other,
                })
                .collect(),
            Inline::Emphasis(inner) => vec![Inline::Emphasis(linkify_inlines(inner))],
            Inline::Strong(inner) => vec![Inline::Strong(linkify_inlines(inner))],
            Inline::Strike(inner) => vec![Inline::Strike(linkify_inlines(inner))],
            // Code spans and existing links are already decided.
            other @ (Inline::Code(_)
            | Inline::Link { .. }
            | Inline::Mention { .. }
            | Inline::SoftBreak) => vec![other],
        })
        .collect()
}

/// Where a URL starts, if there is one, at a word boundary.
///
/// The boundary check stops `nothttp://x` and `mailto:http://x` from being
/// treated as links starting mid-token.
fn find_url(text: &str) -> Option<usize> {
    let mut from = 0;
    while from < text.len() {
        let tail = &text[from..];
        let at = ["https://", "http://"]
            .iter()
            .filter_map(|scheme| tail.find(scheme))
            .min()?;
        let absolute = from + at;
        let preceded_by_word = text[..absolute]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if !preceded_by_word {
            return Some(absolute);
        }
        from = absolute + 1;
    }
    None
}

/// Trim punctuation that reads as sentence punctuation rather than URL.
///
/// A URL at the end of a sentence is normally followed by a full stop, and a
/// URL inside parentheses by a closing bracket that belongs to the prose. A
/// closing bracket is only kept when the URL opened one itself, which is what
/// makes Wikipedia-style links survive.
fn trim_url_tail(url: &str) -> &str {
    let mut end = url.len();
    // `while let` rather than `while end > 0` plus an unwrap: the loop ends
    // naturally when nothing is left, so there is no "cannot happen" branch
    // to leave untested.
    while let Some(last) = url[..end].chars().next_back() {
        let candidate = &url[..end];
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => candidate.matches('(').count() < candidate.matches(')').count(),
            ']' => candidate.matches('[').count() < candidate.matches(']').count(),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= last.len_utf8();
    }
    &url[..end]
}

fn autolink(text: &str) -> Vec<Inline> {
    if find_url(text).is_none() {
        return vec![Inline::Text(text.to_owned())];
    }

    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = find_url(rest) {
        if at > 0 {
            out.push(Inline::Text(rest[..at].to_owned()));
        }
        let tail = &rest[at..];
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let url = trim_url_tail(&tail[..end]);

        // A scheme with nothing after it is not a link.
        let usable = !url.is_empty() && !url.ends_with("//");
        if usable {
            out.push(Inline::Link {
                href: url.to_owned(),
                text: url.to_owned(),
            });
        } else {
            out.push(Inline::Text(tail[..end].to_owned()));
        }
        // Advance past whatever was consumed; anything trimmed off the tail
        // stays in `rest` and comes back as text.
        rest = &tail[if usable { url.len() } else { end }..];
    }
    if !rest.is_empty() {
        out.push(Inline::Text(rest.to_owned()));
    }
    out
}

#[cfg(test)]
#[path = "tests/parse.rs"]
mod tests;

/// Whether a name after an `@` is somebody, and whether it is the reader.
///
/// `None` means it is not a user at all, and the text stays as it was: only
/// real usernames highlight, which is what every other Mattermost client
/// does. An address like `support@example.com` would otherwise light up.
pub type Roster<'a> = dyn Fn(&str) -> Option<Mentioned> + 'a;

/// What a resolved mention refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mentioned {
    /// Somebody else, or a group like `@channel`.
    Someone,
    /// The reader.
    Me,
}

/// Mark the `@mentions` in a parsed message.
///
/// Separate from `parse` because it needs the roster, which `mm-md` has no
/// business holding. The caller passes a closure over the store.
pub fn mark_mentions(doc: &mut Document, roster: &Roster<'_>) {
    for block in &mut doc.blocks {
        mark_block(block, roster);
    }
}

fn mark_block(block: &mut Block, roster: &Roster<'_>) {
    match block {
        Block::Paragraph(inlines) | Block::Heading { inlines, .. } | Block::ListItem(inlines) => {
            let taken = std::mem::take(inlines);
            *inlines = mark_inlines(taken, roster);
        }
        Block::Quote(blocks) => {
            for inner in blocks {
                mark_block(inner, roster);
            }
        }
        // Code is literal, and a rule has no text.
        Block::Code { .. } | Block::Rule => {}
    }
}

fn mark_inlines(inlines: Vec<Inline>, roster: &Roster<'_>) -> Vec<Inline> {
    inlines
        .into_iter()
        .flat_map(|inline| match inline {
            Inline::Text(text) => split_mentions(&text, roster),
            Inline::Emphasis(inner) => vec![Inline::Emphasis(mark_inlines(inner, roster))],
            Inline::Strong(inner) => vec![Inline::Strong(mark_inlines(inner, roster))],
            Inline::Strike(inner) => vec![Inline::Strike(mark_inlines(inner, roster))],
            other => vec![other],
        })
        .collect()
}

/// The characters a username may contain. Mattermost's own rule.
fn is_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

fn split_mentions(text: &str, roster: &Roster<'_>) -> Vec<Inline> {
    if !text.contains('@') {
        return vec![Inline::Text(text.to_owned())];
    }
    let mut out: Vec<Inline> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;

    while let Some(at) = rest.find('@') {
        // An `@` with a word character before it is part of something else
        // -- an email address, a handle inside a word.
        let attached = rest[..at].chars().next_back().is_some_and(is_username_char);
        plain.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        if attached {
            continue;
        }

        let len = rest
            .char_indices()
            .find(|(_, c)| !is_username_char(*c))
            .map_or(rest.len(), |(i, _)| i);
        // Trailing punctuation is usually the sentence's, not the name's, so
        // try progressively shorter candidates -- "@alex." is a mention of
        // "alex" far more often than of "alex.".
        let Some((name, who)) = (0..=len)
            .rev()
            .map(|end| &rest[..end])
            .filter(|candidate| !candidate.is_empty())
            .find_map(|candidate| roster(candidate).map(|who| (candidate, who)))
        else {
            continue;
        };

        plain.pop();
        if !plain.is_empty() {
            out.push(Inline::Text(std::mem::take(&mut plain)));
        }
        out.push(Inline::Mention {
            name: name.to_owned(),
            is_me: who == Mentioned::Me,
        });
        rest = &rest[name.len()..];
    }

    plain.push_str(rest);
    if !plain.is_empty() {
        out.push(Inline::Text(plain));
    }
    out
}
