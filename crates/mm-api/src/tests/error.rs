use super::*;

fn json_error(status: u16, id: &str) -> HttpResponse {
    HttpResponse::new(
        status,
        format!(r#"{{"id":"{id}","message":"boom","status_code":{status}}}"#),
    )
}

#[test]
fn not_modified_is_recognised_and_is_not_a_failure_to_report() {
    assert_eq!(
        ApiError::from_response(&HttpResponse::new(304, "")),
        ApiError::NotModified
    );
}

/// The server returns `http.Error(w, "limit exceeded", 429)` — a plain
/// string, not its usual JSON document. A client that assumes JSON on
/// every error turns a rate limit into a parse error.
#[test]
fn rate_limit_is_recognised_from_a_plain_text_body() {
    let resp = HttpResponse::new(429, "limit exceeded");
    assert_eq!(
        ApiError::from_response(&resp),
        ApiError::RateLimited {
            retry_after_secs: None
        }
    );
}

/// X-Ratelimit-Reset is documented as epoch seconds but implemented as a
/// delta, and Retry-After wins when both are present.
#[test]
fn rate_limit_reads_retry_after_then_ratelimit_reset_as_a_delta() {
    let only_reset = HttpResponse::new(429, "limit exceeded").with_header("X-Ratelimit-Reset", "7");
    assert_eq!(
        ApiError::from_response(&only_reset),
        ApiError::RateLimited {
            retry_after_secs: Some(7)
        }
    );

    let both = HttpResponse::new(429, "limit exceeded")
        .with_header("Retry-After", "2")
        .with_header("X-Ratelimit-Reset", "7");
    assert_eq!(
        ApiError::from_response(&both),
        ApiError::RateLimited {
            retry_after_secs: Some(2)
        }
    );

    let junk = HttpResponse::new(429, "limit exceeded").with_header("Retry-After", "soon");
    assert_eq!(
        ApiError::from_response(&junk),
        ApiError::RateLimited {
            retry_after_secs: None
        }
    );
}

#[test]
fn statuses_map_to_their_variants() {
    assert!(matches!(
        ApiError::from_response(&json_error(401, "api.context.session_expired")),
        ApiError::Unauthorized(_)
    ));
    assert!(matches!(
        ApiError::from_response(&json_error(403, "api.context.permissions")),
        ApiError::Forbidden(_)
    ));
    assert!(matches!(
        ApiError::from_response(&json_error(404, "api.channel.not_found")),
        ApiError::NotFound(_)
    ));
    assert!(matches!(
        ApiError::from_response(&json_error(500, "api.oops")),
        ApiError::Status { status: 500, .. }
    ));
    assert!(matches!(
        ApiError::from_response(&json_error(418, "api.teapot")),
        ApiError::Status { status: 418, .. }
    ));
}

/// A 401 whose id mentions MFA is a prompt for a code, not a rejection, so
/// the caller can re-ask rather than logging the user out.
#[test]
fn mfa_is_distinguished_from_an_ordinary_401() {
    let mfa = json_error(401, "mfa.validate_token.authenticate.app_error");
    assert!(matches!(
        ApiError::from_response(&mfa),
        ApiError::MfaRequired(_)
    ));
}

/// An error body that is not JSON must still produce a useful error, with
/// the text preserved rather than discarded.
#[test]
fn non_json_error_body_is_kept_as_the_message() {
    let resp = HttpResponse::new(502, "<html>bad gateway</html>");
    match ApiError::from_response(&resp) {
        ApiError::Status { status, error } => {
            assert_eq!(status, 502);
            assert_eq!(error.message, "<html>bad gateway</html>");
            assert_eq!(error.status_code, 502);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// Retrying a 401 in a loop is how a client gets its IP rate-limited by
/// the login endpoint, which is itself always rate-limited.
#[test]
fn only_rate_limits_and_server_errors_are_retryable() {
    assert!(
        ApiError::RateLimited {
            retry_after_secs: None
        }
        .is_retryable()
    );
    assert!(ApiError::from_response(&json_error(500, "x")).is_retryable());
    assert!(ApiError::from_response(&json_error(503, "x")).is_retryable());

    assert!(!ApiError::from_response(&json_error(401, "x")).is_retryable());
    assert!(!ApiError::from_response(&json_error(403, "x")).is_retryable());
    assert!(!ApiError::from_response(&json_error(404, "x")).is_retryable());
    assert!(!ApiError::from_response(&json_error(400, "x")).is_retryable());
    assert!(!ApiError::NotModified.is_retryable());
    assert!(!ApiError::malformed("ctx", "d").is_retryable());
}

#[test]
fn describe_prefers_the_stable_id_but_survives_without_it() {
    assert_eq!(
        AppError {
            id: "a.b".into(),
            message: "boom".into(),
            ..AppError::default()
        }
        .describe(),
        "boom (a.b)"
    );
    assert_eq!(
        AppError {
            id: "a.b".into(),
            ..AppError::default()
        }
        .describe(),
        "a.b"
    );
    assert_eq!(
        AppError {
            message: "boom".into(),
            ..AppError::default()
        }
        .describe(),
        "boom"
    );
    assert_eq!(AppError::default().describe(), "no detail");
}

/// Every variant must render, because these strings are what the user is
/// shown when something goes wrong.
#[test]
fn every_variant_renders_non_empty() {
    let e = AppError {
        id: "a.b".into(),
        message: "boom".into(),
        ..AppError::default()
    };
    let all = [
        ApiError::NotModified,
        ApiError::Unauthorized(e.clone()),
        ApiError::MfaRequired(e.clone()),
        ApiError::Forbidden(e.clone()),
        ApiError::NotFound(e.clone()),
        ApiError::RateLimited {
            retry_after_secs: Some(3),
        },
        ApiError::RateLimited {
            retry_after_secs: None,
        },
        ApiError::Status {
            status: 500,
            error: e.clone(),
        },
        ApiError::malformed("posts", "expected an object"),
    ];
    for variant in &all {
        assert!(!variant.to_string().is_empty(), "{variant:?}");
        assert!(!format!("{variant:?}").is_empty());
        assert_eq!(variant, &variant.clone());
        // Exercise the std::error::Error impl too.
        let as_err: &dyn std::error::Error = variant;
        assert!(as_err.source().is_none());
    }
    assert_eq!(e, e.clone());
    assert!(!format!("{e:?}").is_empty());
}
