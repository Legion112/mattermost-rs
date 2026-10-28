//! The request and response types `mm-api` is written against.
//!
//! These are plain data. A builder returns an [`HttpRequest`] and a parser
//! takes an [`HttpResponse`]; neither opens a socket. That is what makes every
//! route, every query-parameter edge case and every error path a table test
//! rather than something that needs a live server.
//!
//! `mm-http` turns these into `reqwest` calls.

use std::fmt;

/// HTTP methods the API uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request body.
///
/// JSON is carried pre-serialized as a string so that a builder stays a pure
/// function of its arguments and its output is directly assertable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Body {
    #[default]
    Empty,
    Json(String),
    /// A multipart upload: the field name, filename, and bytes. Present from
    /// the start even though file upload lands later, because retrofitting a
    /// body variant onto the transport trait afterwards would be a change to
    /// the foundation rather than an addition.
    Multipart {
        fields: Vec<(String, String)>,
        file_field: String,
        filename: String,
        content: Vec<u8>,
    },
    /// Raw bytes with their media type: one piece of an upload session.
    Bytes {
        content_type: String,
        content: Vec<u8>,
    },
}

/// Everything needed to perform one API call, and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpRequest {
    pub method: Method,
    /// Server-relative, always beginning with `/api/v4`.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Request-specific headers. Authentication is added by the transport, not
    /// here, so that no builder has to know about tokens.
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl HttpRequest {
    #[must_use]
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::Get, path)
    }

    #[must_use]
    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::Post, path)
    }

    #[must_use]
    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::Put, path)
    }

    #[must_use]
    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::Delete, path)
    }

    /// Add a query parameter. Values are stored unescaped; the transport is
    /// responsible for encoding, because it owns the URL type.
    #[must_use]
    pub fn q(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.query.push((key.to_owned(), value.to_string()));
        self
    }

    /// Add a query parameter only when it is `Some`, which is the shape most
    /// of the paging and filtering options take.
    #[must_use]
    pub fn q_opt(self, key: &str, value: Option<impl fmt::Display>) -> Self {
        match value {
            Some(v) => self.q(key, v),
            None => self,
        }
    }

    #[must_use]
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Attach an `If-None-Match` header.
    ///
    /// The server compares `ETags` by **exact string equality** and emits them
    /// **unquoted**, so the value must be echoed back byte for byte. Wrapping
    /// it in quotes, as a strict reading of HTTP would suggest, makes every
    /// conditional request miss.
    #[must_use]
    pub fn if_none_match(self, etag: &str) -> Self {
        self.header("If-None-Match", etag)
    }

    /// Attach a JSON body.
    ///
    /// Takes a `Value` rather than `impl Serialize` deliberately. A generic
    /// version has to handle `serde_json::to_string` failing, and that arm is
    /// unreachable for every type this crate actually passes — so it became a
    /// line no test could honestly cover, monomorphized once per call site.
    /// `Value`'s `Display` cannot fail, so the error case stops existing
    /// instead of being papered over.
    #[must_use]
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.body = Body::Json(value.to_string());
        self
    }

    /// A file upload, as the multipart form Mattermost's upload
    /// endpoint takes.
    #[must_use]
    pub fn multipart(
        mut self,
        fields: Vec<(String, String)>,
        file_field: impl Into<String>,
        filename: impl Into<String>,
        content: Vec<u8>,
    ) -> Self {
        self.body = Body::Multipart {
            fields,
            file_field: file_field.into(),
            filename: filename.into(),
            content,
        };
        self
    }

    /// Raw bytes as the body, with their media type: a piece of an upload
    /// session, which takes the file's bytes as they are.
    #[must_use]
    pub fn bytes(mut self, content_type: impl Into<String>, content: Vec<u8>) -> Self {
        self.body = Body::Bytes {
            content_type: content_type.into(),
            content,
        };
        self
    }

    /// The path with its query string appended, for logging and for fakes
    /// keyed by route.
    #[must_use]
    pub fn route_key(&self) -> String {
        if self.query.is_empty() {
            return format!("{} {}", self.method, self.path);
        }
        let q: Vec<String> = self.query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{} {}?{}", self.method, self.path, q.join("&"))
    }
}

/// A response, as received.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Look up a header, case-insensitively.
    ///
    /// Case matters here in practice: the server's constant for the session
    /// token header is the lowercase `token`, and Go canonicalizes it to
    /// `Token` on the wire. A case-sensitive lookup for either spelling works
    /// against one server and not the other.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The server's `ETag`, if it sent one.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.header("ETag")
    }
}

#[cfg(test)]
#[path = "tests/http.rs"]
mod tests;
