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
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                push_plain(out, inner);
            }
            Inline::Link { text, .. } => out.push_str(text),
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
    Link { href: String, text: String },
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

    /// Append an inline to whichever emphasis span is currently open.
    fn push_inline(&mut self, inline: Inline) {
        if let Some((_, inner)) = self.emphasis.last_mut() {
            inner.push(inline);
        } else {
            self.inlines.push(inline);
        }
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
            blocks: self.blocks,
        }
    }
}

#[cfg(test)]
#[path = "tests/parse.rs"]
mod tests;
