//! Message attachments — the Slack-compatible format webhooks post in.
//!
//! An alerting webhook sends a post whose `message` is **empty** and whose
//! content lives entirely in `props.attachments`, with `type` set to
//! `slack_attachment`. A client that renders only `message` draws those
//! posts as an author and a timestamp with nothing under them, which is
//! exactly what an alert channel must not look like.

use crate::wire::nullable;
use serde::{Deserialize, Serialize};

/// One `{title, value}` pair under an attachment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentField {
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    /// Slack allows a number or a boolean here, not only a string, so this
    /// is kept as raw JSON and rendered by `text`.
    #[serde(deserialize_with = "nullable")]
    pub value: serde_json::Value,
    /// Whether it may share a line with the next one.
    #[serde(deserialize_with = "nullable")]
    pub short: bool,
}

impl AttachmentField {
    /// The value as something displayable, without JSON's quotes around a
    /// string.
    #[must_use]
    pub fn text(&self) -> String {
        match &self.value {
            serde_json::Value::Null => String::new(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

/// A Slack-compatible attachment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageAttachment {
    /// A bar down the left-hand side. Either a hex colour or one of Slack's
    /// three names.
    #[serde(deserialize_with = "nullable")]
    pub color: String,
    /// Shown above the attachment, outside its bar.
    #[serde(deserialize_with = "nullable")]
    pub pretext: String,
    #[serde(deserialize_with = "nullable")]
    pub author_name: String,
    #[serde(deserialize_with = "nullable")]
    pub author_link: String,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub title_link: String,
    /// Markdown, like a message body.
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    #[serde(deserialize_with = "nullable")]
    pub fields: Vec<AttachmentField>,
    #[serde(deserialize_with = "nullable")]
    pub image_url: String,
    #[serde(deserialize_with = "nullable")]
    pub thumb_url: String,
    #[serde(deserialize_with = "nullable")]
    pub footer: String,
    /// What a client that cannot render attachments should show instead.
    /// Worth keeping: it is the one field guaranteed to say something.
    #[serde(deserialize_with = "nullable")]
    pub fallback: String,
}

impl MessageAttachment {
    /// Whether this would draw as nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pretext.is_empty()
            && self.author_name.is_empty()
            && self.title.is_empty()
            && self.text.is_empty()
            && self.fields.is_empty()
            && self.footer.is_empty()
    }

    /// The bar's colour as 8-bit RGB, if it states one.
    ///
    /// Slack's three names are accepted alongside hex because webhooks send
    /// them -- a real alerting integration posts `"color": "warning"`, not a
    /// hex triple. An unparseable value gets no bar rather than a black one.
    #[must_use]
    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        match self.color.trim() {
            "good" => Some((0x3d, 0xb8, 0x87)),
            "warning" => Some((0xff, 0xbc, 0x1f)),
            "danger" => Some((0xd2, 0x4b, 0x4e)),
            other => parse_hex(other),
        }
    }
}

/// `#rgb` or `#rrggbb`, with or without the hash.
fn parse_hex(raw: &str) -> Option<(u8, u8, u8)> {
    let digits = raw.strip_prefix('#').unwrap_or(raw);
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match digits.len() {
        // The short form doubles each digit: #abc is #aabbcc.
        3 => {
            let mut out = [0u8; 3];
            for (slot, c) in out.iter_mut().zip(digits.chars()) {
                let v = byte(&c.to_string())?;
                *slot = v * 17;
            }
            Some((out[0], out[1], out[2]))
        }
        6 => Some((
            byte(&digits[0..2])?,
            byte(&digits[2..4])?,
            byte(&digits[4..6])?,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slack's three names, which is what a real alerting webhook sends --
    /// `"color": "warning"`, never a hex triple.
    #[test]
    fn the_named_colours_are_understood() {
        for name in ["good", "warning", "danger"] {
            let a = MessageAttachment {
                color: name.into(),
                ..MessageAttachment::default()
            };
            assert!(a.rgb().is_some(), "{name}");
        }
    }

    /// Both hex forms, with and without the hash.
    #[test]
    fn hex_colours_parse_in_both_lengths() {
        let colour = |raw: &str| {
            MessageAttachment {
                color: raw.into(),
                ..MessageAttachment::default()
            }
            .rgb()
        };
        assert_eq!(colour("#ff0000"), Some((255, 0, 0)));
        assert_eq!(colour("00ff00"), Some((0, 255, 0)));
        // The short form doubles each digit, so #abc is #aabbcc.
        assert_eq!(colour("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(colour("  #0000ff  "), Some((0, 0, 255)));
    }

    /// Anything else draws no bar rather than a black one, which would read
    /// as a deliberate colour.
    #[test]
    fn an_unparseable_colour_is_no_colour() {
        for raw in ["", "chartreuse", "#12", "#gg0000", "#1234567"] {
            let a = MessageAttachment {
                color: raw.into(),
                ..MessageAttachment::default()
            };
            assert_eq!(a.rgb(), None, "{raw:?}");
        }
    }

    /// Slack allows a number or a boolean as a field value, and JSON's
    /// quotes must not reach the screen for a string.
    #[test]
    fn a_field_value_renders_whatever_type_it_arrived_as() {
        let field = |v: serde_json::Value| AttachmentField {
            value: v,
            ..AttachmentField::default()
        };
        assert_eq!(field(serde_json::json!("four")).text(), "four");
        assert_eq!(field(serde_json::json!(4)).text(), "4");
        assert_eq!(field(serde_json::json!(true)).text(), "true");
        assert_eq!(field(serde_json::Value::Null).text(), "");
    }

    /// An attachment with nothing in it must not draw an empty bar.
    #[test]
    fn an_attachment_with_no_content_is_empty() {
        assert!(MessageAttachment::default().is_empty());
        // A colour alone is not content.
        assert!(
            MessageAttachment {
                color: "danger".into(),
                ..MessageAttachment::default()
            }
            .is_empty()
        );
        assert!(
            !MessageAttachment {
                text: "something".into(),
                ..MessageAttachment::default()
            }
            .is_empty()
        );
    }
}

#[cfg(test)]
mod null_tests {
    use super::*;

    /// Captured verbatim from a webhook post on 12.0.1. Every unset field is
    /// present and **null**, not omitted — and `#[serde(default)]` does not
    /// cover that. Before this, the `"fields": null` alone made the whole
    /// attachment list fail to parse, so an alert channel rendered as rows
    /// with an author, a timestamp and nothing else.
    const LIVE: &str = r#"{
      "author_icon":"","author_link":"","author_name":"","color":"danger",
      "fallback":"","fields":null,"footer":"","footer_icon":"","id":0,
      "image_url":"","pretext":"","text":"posted while the window was open",
      "thumb_url":"","title":"[FIRING:2] ThirdAlert","title_link":"","ts":null
    }"#;

    #[test]
    fn an_attachment_whose_unset_fields_are_null_still_parses() {
        let a: MessageAttachment = serde_json::from_str(LIVE).expect("parses");
        assert_eq!(a.title, "[FIRING:2] ThirdAlert");
        assert_eq!(a.text, "posted while the window was open");
        assert!(a.fields.is_empty());
        assert_eq!(a.rgb(), Some((0xd2, 0x4b, 0x4e)));
        assert!(!a.is_empty(), "it has a title and text, so it draws");
    }

    /// And null on every single field, which is what an attachment with
    /// nothing set at all comes back as.
    #[test]
    fn null_everywhere_is_an_empty_attachment_rather_than_an_error() {
        let all_null = r#"{"color":null,"pretext":null,"author_name":null,
            "author_link":null,"title":null,"title_link":null,"text":null,
            "fields":null,"image_url":null,"thumb_url":null,"footer":null,
            "fallback":null}"#;
        let a: MessageAttachment = serde_json::from_str(all_null).expect("parses");
        assert!(a.is_empty());
        assert_eq!(a.rgb(), None);
    }

    /// A field inside the list can be null too.
    #[test]
    fn a_null_inside_a_field_is_tolerated() {
        let json = r#"{"fields":[{"title":null,"value":null,"short":null}]}"#;
        let a: MessageAttachment = serde_json::from_str(json).expect("parses");
        assert_eq!(a.fields.len(), 1);
        assert_eq!(a.fields[0].title, "");
        assert_eq!(a.fields[0].text(), "");
        assert!(!a.fields[0].short);
    }
}
