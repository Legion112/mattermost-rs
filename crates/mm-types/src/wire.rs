//! Helpers for the places where Mattermost's wire format is surprising.

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};

/// Deserialize a field whose value is a **JSON document inside a JSON string**.
///
/// This is the single most common way a Mattermost client gets written wrong.
/// On the WebSocket, most interesting payloads are not nested objects — the
/// server calls `.ToJSON()` on the model and puts the resulting *string* into
/// the data map. Confirmed against the server for: `post` (on `posted`,
/// `post_edited`, `post_deleted`, `ephemeral_message`), `channel`,
/// `channelMember` (note the camelCase key), `thread`, `reaction`,
/// `preferences`, `emoji` and `teammate_ids`.
///
/// The exceptions, which are ordinary nested values and must *not* use this:
/// `user` on `user_updated`, and `channel_times` on
/// `multiple_channels_viewed`.
pub fn json_string<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = String::deserialize(de)?;
    serde_json::from_str(&raw).map_err(|e| {
        D::Error::custom(format!(
            "field is a JSON-encoded string but its contents did not parse: {e}"
        ))
    })
}

/// Same, for an optional field.
pub fn json_string_opt<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let Some(raw) = Option::<String>::deserialize(de)? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw).map(Some).map_err(|e| {
        D::Error::custom(format!(
            "field is a JSON-encoded string but its contents did not parse: {e}"
        ))
    })
}

/// Read a field that the server may send as `null`, as its default.
///
/// `#[serde(default)]` covers a **missing** field and nothing else: a field
/// that is present and null still fails, and the failure takes the whole
/// surrounding structure with it. Mattermost serialises unset fields of an
/// attachment as `null` rather than omitting them — `"fields": null`,
/// `"ts": null` — so a webhook post parsed to nothing at all and alert
/// channels rendered as empty rows.
pub fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Inner {
        id: String,
        n: i64,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Outer {
        #[serde(deserialize_with = "json_string")]
        payload: Inner,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Maybe {
        #[serde(default, deserialize_with = "json_string_opt")]
        payload: Option<Inner>,
    }

    /// The wart itself: the value is a string, and inside it is the object.
    #[test]
    fn parses_a_json_document_out_of_a_string() {
        let outer: Outer = serde_json::from_str(r#"{"payload":"{\"id\":\"a\",\"n\":3}"}"#).unwrap();
        assert_eq!(
            outer.payload,
            Inner {
                id: "a".into(),
                n: 3
            }
        );
    }

    /// A nested object where a JSON-encoded string was expected is exactly the
    /// mistake this helper exists to catch, so it must fail loudly rather than
    /// silently producing a default.
    #[test]
    fn rejects_a_nested_object() {
        let err = serde_json::from_str::<Outer>(r#"{"payload":{"id":"a","n":3}}"#).unwrap_err();
        assert!(err.to_string().contains("string"), "{err}");
    }

    #[test]
    fn reports_bad_inner_json_with_context() {
        let err = serde_json::from_str::<Outer>(r#"{"payload":"{not json}"}"#).unwrap_err();
        assert!(
            err.to_string().contains("JSON-encoded string"),
            "unhelpful message: {err}"
        );
    }

    #[test]
    fn optional_handles_absent_null_and_empty() {
        for src in [r"{}", r#"{"payload":null}"#, r#"{"payload":""}"#] {
            let m: Maybe = serde_json::from_str(src).unwrap();
            assert_eq!(m.payload, None, "{src}");
        }
    }

    #[test]
    fn optional_parses_when_present() {
        let m: Maybe = serde_json::from_str(r#"{"payload":"{\"id\":\"b\",\"n\":1}"}"#).unwrap();
        assert_eq!(
            m.payload,
            Some(Inner {
                id: "b".into(),
                n: 1
            })
        );
    }

    #[test]
    fn optional_reports_bad_inner_json() {
        let err = serde_json::from_str::<Maybe>(r#"{"payload":"{oops}"}"#).unwrap_err();
        assert!(err.to_string().contains("JSON-encoded string"), "{err}");
    }
}
