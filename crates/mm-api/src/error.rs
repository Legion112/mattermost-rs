//! What can come back instead of the thing you asked for.

use crate::http::HttpResponse;
use serde::Deserialize;
use std::fmt;

/// The server's error document.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppError {
    /// A stable identifier such as `api.user.login.invalid_credentials`. This
    /// is the field to branch on; `message` is localized prose.
    pub id: String,
    pub message: String,
    pub detailed_error: String,
    pub request_id: String,
    pub status_code: u16,
}

/// Everything an API call can fail with.
///
/// Every variant here is reachable through `testing::RecordingHttp`, which is
/// deliberate: a variant that cannot be produced by the fake cannot be tested,
/// and under a 100% coverage gate an untestable variant is a design smell
/// rather than an inconvenience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 304. Not a failure — the cached copy is still current.
    NotModified,
    /// 401 with no session, or a rejected token.
    Unauthorized(AppError),
    /// 401 where the server is asking for a multi-factor code.
    ///
    /// The exact discriminator is **not confirmed** against a live server; it
    /// is recognised here by `mfa` appearing in the error id. Verify before
    /// relying on it, and see `docs/parity.toml`.
    MfaRequired(AppError),
    /// 403.
    Forbidden(AppError),
    /// 404.
    NotFound(AppError),
    /// 429. Note that the body is **not** JSON — see [`ApiError::from_response`].
    RateLimited {
        /// From `Retry-After`, or from `X-Ratelimit-Reset`, in seconds.
        retry_after_secs: Option<u64>,
    },
    /// Any other non-2xx with a parseable error document.
    Status { status: u16, error: AppError },
    /// A 2xx whose body was not what this endpoint promises.
    Malformed {
        context: &'static str,
        detail: String,
    },
}

impl ApiError {
    /// Classify a non-2xx response.
    ///
    /// The 429 case is the one worth reading. The server does **not** return
    /// its usual JSON error document for a rate limit — the handler calls
    /// `http.Error(w, "limit exceeded", 429)`, so the body is that plain
    /// string. Any client that assumes a JSON body on every error turns a rate
    /// limit into a parse error and reports the wrong thing.
    ///
    /// `X-Ratelimit-Reset` is documented as "remaining UTC epoch seconds" but
    /// the implementation emits `ceil(ResetAfter.Seconds())`, a delta. It is
    /// read as a delta here.
    #[must_use]
    pub fn from_response(resp: &HttpResponse) -> Self {
        if resp.status == 304 {
            return Self::NotModified;
        }
        if resp.status == 429 {
            return Self::RateLimited {
                retry_after_secs: resp
                    .header("Retry-After")
                    .or_else(|| resp.header("X-Ratelimit-Reset"))
                    .and_then(|v| v.trim().parse().ok()),
            };
        }

        let error = serde_json::from_slice::<AppError>(&resp.body).unwrap_or_else(|_| AppError {
            message: resp.body_str().trim().to_owned(),
            status_code: resp.status,
            ..AppError::default()
        });

        match resp.status {
            401 if error.id.contains("mfa") => Self::MfaRequired(error),
            401 => Self::Unauthorized(error),
            403 => Self::Forbidden(error),
            404 => Self::NotFound(error),
            status => Self::Status { status, error },
        }
    }

    /// A malformed-body error, for parsers.
    #[must_use]
    pub fn malformed(context: &'static str, detail: impl fmt::Display) -> Self {
        Self::Malformed {
            context,
            detail: detail.to_string(),
        }
    }

    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// Used by the engine to decide between backing off and surfacing the
    /// failure. A 401 is deliberately *not* retryable: retrying it in a loop
    /// is how a client gets its IP rate-limited by the login endpoint.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Status { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotModified => f.write_str("not modified"),
            Self::Unauthorized(e) => write!(f, "unauthorized: {}", e.describe()),
            Self::MfaRequired(e) => write!(f, "multi-factor code required: {}", e.describe()),
            Self::Forbidden(e) => write!(f, "forbidden: {}", e.describe()),
            Self::NotFound(e) => write!(f, "not found: {}", e.describe()),
            Self::RateLimited { retry_after_secs } => match retry_after_secs {
                Some(s) => write!(f, "rate limited, retry in {s}s"),
                None => f.write_str("rate limited"),
            },
            Self::Status { status, error } => {
                write!(f, "server returned {status}: {}", error.describe())
            }
            Self::Malformed { context, detail } => {
                write!(f, "malformed response for {context}: {detail}")
            }
        }
    }
}

impl std::error::Error for ApiError {}

impl AppError {
    /// The most useful one-line rendering available, preferring the stable id.
    #[must_use]
    pub fn describe(&self) -> String {
        match (self.id.is_empty(), self.message.is_empty()) {
            (false, false) => format!("{} ({})", self.message, self.id),
            (false, true) => self.id.clone(),
            (true, false) => self.message.clone(),
            (true, true) => "no detail".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "tests/error.rs"]
mod tests;
