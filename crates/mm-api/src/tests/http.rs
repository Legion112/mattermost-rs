use super::*;

#[test]
fn methods_render_as_their_wire_names() {
    for (m, s) in [
        (Method::Get, "GET"),
        (Method::Post, "POST"),
        (Method::Put, "PUT"),
        (Method::Patch, "PATCH"),
        (Method::Delete, "DELETE"),
    ] {
        assert_eq!(m.as_str(), s);
        assert_eq!(m.to_string(), s);
    }
    assert_eq!(Method::default(), Method::Get);
}

#[test]
fn constructors_set_the_method_and_path() {
    assert_eq!(HttpRequest::get("/a").method, Method::Get);
    assert_eq!(HttpRequest::post("/a").method, Method::Post);
    assert_eq!(HttpRequest::put("/a").method, Method::Put);
    assert_eq!(HttpRequest::delete("/a").method, Method::Delete);
    assert_eq!(HttpRequest::new(Method::Patch, "/a").method, Method::Patch);
    assert_eq!(HttpRequest::get("/a").path, "/a");
}

#[test]
fn query_parameters_accumulate_in_order() {
    let req = HttpRequest::get("/p").q("page", 0).q("per_page", 60);
    assert_eq!(
        req.query,
        vec![
            ("page".to_owned(), "0".to_owned()),
            ("per_page".to_owned(), "60".to_owned())
        ]
    );
}

/// Most paging options are optional, and a `None` must not become
/// `?since=` with an empty value — the server would parse that as zero and
/// return the entire channel.
#[test]
fn optional_query_parameters_are_omitted_when_absent() {
    let req = HttpRequest::get("/p")
        .q_opt("since", None::<i64>)
        .q_opt("before", Some("p1"));
    assert_eq!(req.query, vec![("before".to_owned(), "p1".to_owned())]);
}

/// The server compares `ETags` by exact string equality and emits them
/// unquoted, so adding quotes would make every conditional request miss.
#[test]
fn if_none_match_sends_the_etag_verbatim() {
    let req = HttpRequest::get("/p").if_none_match("10.6.0.abc.true");
    assert_eq!(
        req.headers,
        vec![("If-None-Match".to_owned(), "10.6.0.abc.true".to_owned())]
    );
}

#[test]
fn json_bodies_are_serialized_eagerly() {
    let req = HttpRequest::post("/p").json(&serde_json::json!({"a": 1}));
    assert_eq!(req.body, Body::Json("{\"a\":1}".to_owned()));
    assert_eq!(HttpRequest::get("/p").body, Body::Empty);
}

/// `json` takes a `Value`, whose rendering cannot fail, so there is no
/// "serialization failed" case to test any more — which is the point. An
/// earlier generic version had an unreachable error arm that no test could
/// honestly cover.
#[test]
fn json_handles_every_value_shape_without_a_failure_mode() {
    for value in [
        serde_json::json!({}),
        serde_json::json!({"a": 1, "b": [true, null]}),
        serde_json::json!([]),
        serde_json::Value::Null,
        serde_json::json!("bare string"),
    ] {
        let req = HttpRequest::post("/p").json(&value);
        assert_eq!(req.body, Body::Json(value.to_string()));
    }
}

#[test]
fn route_key_includes_the_query_only_when_there_is_one() {
    assert_eq!(HttpRequest::get("/p").route_key(), "GET /p");
    assert_eq!(
        HttpRequest::get("/p").q("a", 1).q("b", 2).route_key(),
        "GET /p?a=1&b=2"
    );
}

/// The session token header is `token` in the server's own constant and
/// `Token` on the wire, so the lookup has to be case-insensitive or it
/// works against one and not the other.
#[test]
fn header_lookup_ignores_case() {
    let resp = HttpResponse::new(200, "").with_header("Token", "abc");
    assert_eq!(resp.header("token"), Some("abc"));
    assert_eq!(resp.header("TOKEN"), Some("abc"));
    assert_eq!(resp.header("Token"), Some("abc"));
    assert_eq!(resp.header("other"), None);
}

#[test]
fn success_covers_exactly_the_2xx_range() {
    for status in [200_u16, 201, 204, 299] {
        assert!(HttpResponse::new(status, "").is_success(), "{status}");
    }
    for status in [199_u16, 300, 304, 400, 500] {
        assert!(!HttpResponse::new(status, "").is_success(), "{status}");
    }
}

#[test]
fn etag_and_body_accessors_work() {
    let resp = HttpResponse::new(200, "hi").with_header("ETag", "v1");
    assert_eq!(resp.etag(), Some("v1"));
    assert_eq!(resp.body_str(), "hi");
    assert_eq!(HttpResponse::new(200, "").etag(), None);
}

/// Invalid UTF-8 from a server must not panic the client.
#[test]
fn body_str_is_lossy_rather_than_panicking() {
    let resp = HttpResponse::new(200, vec![0xff, 0xfe]);
    assert!(!resp.body_str().is_empty());
}

#[test]
fn multipart_body_is_representable() {
    let body = Body::Multipart {
        fields: vec![("channel_id".to_owned(), "c1".to_owned())],
        file_field: "files".to_owned(),
        filename: "a.png".to_owned(),
        content: vec![1, 2, 3],
    };
    let mut req = HttpRequest::post("/api/v4/files");
    req.body = body.clone();
    assert_eq!(req.body, body);
}

#[test]
fn every_derive_is_exercised() {
    let req = HttpRequest::get("/p").header("X", "y");
    let resp = HttpResponse::new(200, "b");
    assert_eq!(req, req.clone());
    assert_eq!(resp, resp.clone());
    assert_eq!(Body::default(), Body::Empty);
    assert_eq!(HttpRequest::default().path, "");
    assert_eq!(HttpResponse::default().status, 0);
    for s in [
        format!("{req:?}"),
        format!("{resp:?}"),
        format!("{:?}", Method::Get),
        format!("{:?}", Body::Empty),
    ] {
        assert!(!s.is_empty());
    }
}

#[test]
fn a_bytes_body_carries_its_media_type() {
    let req = HttpRequest::post("/api/v4/uploads/u1").bytes("application/octet-stream", vec![9, 8]);
    match &req.body {
        Body::Bytes {
            content_type,
            content,
        } => {
            assert_eq!(content_type, "application/octet-stream");
            assert_eq!(content, &[9, 8]);
        }
        other => panic!("expected bytes, got {other:?}"),
    }
}
