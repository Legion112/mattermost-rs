use super::*;
use crate::http::{Body, Method};

fn ok(body: &str) -> HttpResponse {
    HttpResponse::new(200, body)
}

// ------------------------------------------------------------ auth

#[test]
fn login_sends_a_flat_map_of_strings() {
    let req = login(&Login {
        login_id: "jane".into(),
        password: "hunter2".into(),
        mfa_token: None,
    });
    assert_eq!(req.method, Method::Post);
    assert_eq!(req.path, "/api/v4/users/login");
    assert_eq!(
        req.body,
        Body::Json(r#"{"login_id":"jane","password":"hunter2"}"#.to_owned())
    );
}

/// The MFA field is named `token`. Calling it `mfa_token` on the wire
/// silently fails the second factor.
#[test]
fn login_names_the_mfa_field_token() {
    let req = login(&Login {
        login_id: "jane".into(),
        password: "hunter2".into(),
        mfa_token: Some("123456".into()),
    });
    let Body::Json(body) = &req.body else {
        panic!("expected a JSON body")
    };
    assert!(body.contains(r#""token":"123456""#), "{body}");
    assert!(!body.contains("mfa_token"), "{body}");
}

/// The token is in the `Token` *header*. A client that looks in the body
/// sees a successful login and then fails every subsequent call.
#[test]
fn parse_login_takes_the_token_from_the_header() {
    let resp = ok(r#"{"id":"u1","username":"jane"}"#).with_header("Token", "sess-abc");
    let session = parse_login(&resp).unwrap();
    assert_eq!(session.token, "sess-abc");
    assert_eq!(session.user.username, "jane");
}

#[test]
fn parse_login_accepts_the_lowercase_header_spelling() {
    let resp = ok(r#"{"id":"u1"}"#).with_header("token", "sess-abc");
    assert_eq!(parse_login(&resp).unwrap().token, "sess-abc");
}

#[test]
fn parse_login_explains_a_missing_or_empty_token_header() {
    let missing = parse_login(&ok(r#"{"id":"u1"}"#)).unwrap_err();
    assert!(missing.to_string().contains("Token header"), "{missing}");

    let empty = parse_login(&ok(r#"{"id":"u1"}"#).with_header("Token", "")).unwrap_err();
    assert!(empty.to_string().contains("empty"), "{empty}");
}

#[test]
fn parse_login_surfaces_bad_credentials_as_unauthorized() {
    let resp = HttpResponse::new(
        401,
        r#"{"id":"api.user.login.invalid_credentials_email_username","message":"no"}"#,
    );
    assert!(matches!(parse_login(&resp), Err(ApiError::Unauthorized(_))));
}

#[test]
fn logout_and_me_hit_their_paths() {
    assert_eq!(logout().path, "/api/v4/users/logout");
    assert_eq!(me().path, "/api/v4/users/me");
    assert_eq!(me().method, Method::Get);
    let u = parse_user(&ok(r#"{"id":"u1","username":"jane"}"#)).unwrap();
    assert_eq!(u.username, "jane");
}

#[test]
fn parse_user_reports_a_malformed_body_with_context() {
    let err = parse_user(&ok("not json")).unwrap_err();
    match err {
        ApiError::Malformed { context, .. } => assert_eq!(context, "GET /users/me"),
        other => panic!("unexpected: {other:?}"),
    }
}

// ------------------------------------------------------------ system

/// Plain /ping says OK while the database is down, so the builder always
/// asks for the server status.
#[test]
fn ping_always_requests_the_server_status() {
    let req = ping();
    assert_eq!(
        req.query,
        vec![("get_server_status".to_owned(), "true".to_owned())]
    );
}

#[test]
fn ping_readiness_requires_the_database_when_it_is_reported() {
    let healthy = parse_ping(&ok(r#"{"status":"OK","database_status":"OK"}"#)).unwrap();
    assert!(healthy.is_healthy() && healthy.is_ready());

    let db_down = parse_ping(&ok(r#"{"status":"OK","database_status":"UNHEALTHY"}"#)).unwrap();
    assert!(db_down.is_healthy(), "the server itself answers");
    assert!(!db_down.is_ready(), "but it cannot serve data");

    let quiet = parse_ping(&ok(r#"{"status":"OK"}"#)).unwrap();
    assert!(quiet.is_ready(), "an absent field is not a failure");

    let unhealthy = parse_ping(&ok(r#"{"status":"UNHEALTHY"}"#)).unwrap();
    assert!(!unhealthy.is_healthy() && !unhealthy.is_ready());
    assert!(!Ping::default().is_healthy());
}

/// Every value in the client config is a string, numbers included.
#[test]
fn client_config_values_are_all_strings() {
    let req = client_config();
    assert_eq!(req.query, vec![("format".to_owned(), "old".to_owned())]);
    let cfg = parse_client_config(&ok(
        r#"{"MaxFileSize":"104857600","EnableCustomEmoji":"true"}"#,
    ))
    .unwrap();
    assert_eq!(cfg["MaxFileSize"], "104857600");
    assert_eq!(cfg["EnableCustomEmoji"], "true");
}

// ------------------------------------------------------------ channels

#[test]
fn team_and_channel_paths_interpolate_the_ids() {
    let team = TeamId::new("t1");
    assert_eq!(my_teams().path, "/api/v4/users/me/teams");
    assert_eq!(
        my_channels(&team).path,
        "/api/v4/users/me/teams/t1/channels"
    );
    assert_eq!(
        my_channel_members(&team).path,
        "/api/v4/users/me/teams/t1/channels/members"
    );
    assert_eq!(parse_teams(&ok(r#"[{"id":"t1"}]"#)).unwrap().len(), 1);
    assert_eq!(parse_channels(&ok(r#"[{"id":"c1"}]"#)).unwrap().len(), 1);
    assert_eq!(
        parse_channel_members(&ok(r#"[{"channel_id":"c1"}]"#))
            .unwrap()
            .len(),
        1
    );
}

/// The categories endpoint is addressed by explicit user id -- there is no
/// `/users/me/` form for it, and assuming there was returns a 404 that looks
/// like the user having no categories.
#[test]
fn the_categories_path_carries_the_user_id_not_me() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    assert_eq!(
        my_categories(&user, &team).path,
        "/api/v4/users/u1/teams/t1/channels/categories"
    );
    let list = parse_categories(&ok(
        r#"{"categories":[{"id":"channels_u1_t1","type":"channels","sorting":"",
            "display_name":"Channels","channel_ids":["c1"]}],
            "order":["channels_u1_t1"]}"#,
    ))
    .unwrap();
    assert_eq!(list.categories.len(), 1);
    assert_eq!(list.order, ["channels_u1_t1"]);
}

/// The stats endpoint feeds the member count in the channel header.
#[test]
fn channel_stats_reads_the_member_count() {
    let c = ChannelId::new("c1");
    assert_eq!(channel_stats(&c).path, "/api/v4/channels/c1/stats");
    let stats = parse_channel_stats(&ok(
        r#"{"channel_id":"c1","member_count":4,"pinnedpost_count":1}"#,
    ))
    .unwrap();
    assert_eq!(stats.member_count, 4);
    assert_eq!(stats.pinned_post_count, 1);
}

/// An empty channel id is a legal, meaningful body meaning "focus loss".
#[test]
fn view_channel_allows_an_empty_channel_for_focus_loss() {
    let c = ChannelId::new("c1");
    let focused = view_channel(Some(&c), None, true);
    let Body::Json(body) = &focused.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""channel_id":"c1""#), "{body}");
    assert!(
        body.contains(r#""collapsed_threads_supported":true"#),
        "{body}"
    );

    let blurred = view_channel(None, Some(&c), false);
    let Body::Json(body) = &blurred.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""channel_id":"""#), "{body}");
    assert!(body.contains(r#""prev_channel_id":"c1""#), "{body}");
    assert_eq!(focused.path, "/api/v4/channels/members/me/view");
}

#[test]
fn view_channel_response_parses() {
    let r = parse_view_channel(&ok(
        r#"{"status":"OK","last_viewed_at_times":{"c1":1700000000000}}"#,
    ))
    .unwrap();
    assert_eq!(r.status, "OK");
    assert_eq!(r.last_viewed_at_times["c1"], 1_700_000_000_000);
    assert_eq!(ViewResult::default().status, "");
}

// ------------------------------------------------------------ posts

/// The four selectors are mutually exclusive on the server, so each one
/// must produce exactly its own parameter and no others.
#[test]
fn each_post_query_sends_only_its_own_parameters() {
    let c = ChannelId::new("c1");

    let page = posts_for_channel(&c, &PostQuery::first());
    assert_eq!(page.path, "/api/v4/channels/c1/posts");
    assert_eq!(
        page.query,
        vec![
            ("page".to_owned(), "0".to_owned()),
            ("per_page".to_owned(), "60".to_owned())
        ]
    );

    let before = posts_for_channel(
        &c,
        &PostQuery::Before {
            post: "p9".into(),
            per_page: 30,
        },
    );
    assert_eq!(
        before.query,
        vec![
            ("before".to_owned(), "p9".to_owned()),
            ("per_page".to_owned(), "30".to_owned())
        ]
    );

    let after = posts_for_channel(
        &c,
        &PostQuery::After {
            post: "p1".into(),
            per_page: 30,
        },
    );
    assert_eq!(
        after.query,
        vec![
            ("after".to_owned(), "p1".to_owned()),
            ("per_page".to_owned(), "30".to_owned())
        ]
    );

    let since = posts_for_channel(&c, &PostQuery::Since { millis: 1_700 });
    assert_eq!(
        since.query,
        vec![("since".to_owned(), "1700".to_owned())],
        "since takes no per_page: it is capped at 1000 server-side regardless"
    );
}

/// Asking for more than 200 gets silently truncated by the server, which
/// looks like a full page and is not one.
#[test]
fn per_page_is_clamped_to_what_the_server_honours() {
    let c = ChannelId::new("c1");
    let huge = posts_for_channel(
        &c,
        &PostQuery::Page {
            page: 0,
            per_page: 1_000,
        },
    );
    assert_eq!(huge.query[1], ("per_page".to_owned(), "200".to_owned()));

    let zero = posts_for_channel(
        &c,
        &PostQuery::Page {
            page: 0,
            per_page: 0,
        },
    );
    assert_eq!(zero.query[1], ("per_page".to_owned(), "1".to_owned()));

    // Clamping must not disturb the since variant, which has no per_page.
    assert_eq!(
        PostQuery::Since { millis: 5 }.clamped(),
        PostQuery::Since { millis: 5 }
    );
    assert_eq!(
        PostQuery::Before {
            post: "p".into(),
            per_page: 999
        }
        .clamped(),
        PostQuery::Before {
            post: "p".into(),
            per_page: 200
        }
    );
    assert_eq!(
        PostQuery::After {
            post: "p".into(),
            per_page: 999
        }
        .clamped(),
        PostQuery::After {
            post: "p".into(),
            per_page: 200
        }
    );
}

#[test]
fn post_list_and_thread_parse() {
    let list = parse_post_list(&ok(
        r#"{"order":["p1"],"posts":{"p1":{"id":"p1","message":"hi"}}}"#,
    ))
    .unwrap();
    assert_eq!(list.order.len(), 1);
    assert_eq!(post_thread("p1").path, "/api/v4/posts/p1/thread");
}

/// Without `pending_post_id` an optimistic post duplicates on screen and
/// then jumps when the server's own copy arrives.
#[test]
fn create_post_carries_a_pending_id_and_omits_an_empty_root() {
    let c = ChannelId::new("c1");
    let root = create_post(&c, "hello", None, "pending-1");
    let Body::Json(body) = &root.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""pending_post_id":"pending-1""#), "{body}");
    assert!(
        !body.contains("root_id"),
        "a root post sends no root_id: {body}"
    );

    let reply = create_post(&c, "hello", Some("p1"), "pending-2");
    let Body::Json(body) = &reply.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""root_id":"p1""#), "{body}");
    assert_eq!(root.path, "/api/v4/posts");

    let created = parse_post(&ok(r#"{"id":"p2","message":"hello"}"#)).unwrap();
    assert_eq!(created.message, "hello");
}

// ------------------------------------------------------------ misc

#[test]
fn users_by_ids_posts_a_bare_array() {
    let req = users_by_ids(&["u1".to_owned(), "u2".to_owned()]);
    assert_eq!(req.path, "/api/v4/users/ids");
    assert_eq!(req.body, Body::Json(r#"["u1","u2"]"#.to_owned()));
    assert_eq!(parse_users(&ok(r#"[{"id":"u1"}]"#)).unwrap().len(), 1);
}

#[test]
fn preferences_round_trip() {
    assert_eq!(my_preferences().path, "/api/v4/users/me/preferences");
    let prefs = parse_preferences(&ok(
            r#"[{"user_id":"u1","category":"display_settings","name":"name_format","value":"full_name"}]"#,
        ))
        .unwrap();
    assert_eq!(prefs[0].value, "full_name");
}

/// Every parser must route a non-2xx through `ApiError` rather than trying
/// to deserialize the error document as the success type.
#[test]
fn every_parser_propagates_a_server_error() {
    let err = HttpResponse::new(403, r#"{"id":"api.no","message":"nope"}"#);
    assert!(matches!(parse_user(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(parse_teams(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(parse_channels(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(
        parse_channel_members(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(parse_post_list(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(parse_post(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(parse_users(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(
        parse_preferences(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(parse_ping(&err), Err(ApiError::Forbidden(_))));
    assert!(matches!(
        parse_categories(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(
        parse_channel_stats(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(
        parse_client_config(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(
        parse_view_channel(&err),
        Err(ApiError::Forbidden(_))
    ));
    assert!(matches!(parse_login(&err), Err(ApiError::Forbidden(_))));
}

/// A 304 must reach the caller as `NotModified` so a cached copy can be
/// kept, not as a malformed-body error.
#[test]
fn not_modified_reaches_the_caller() {
    let resp = HttpResponse::new(304, "");
    assert!(matches!(parse_channels(&resp), Err(ApiError::NotModified)));
}

#[test]
fn every_derive_is_exercised() {
    let l = Login::default();
    let q = PostQuery::first();
    let p = Ping::default();
    let v = ViewResult::default();
    assert_eq!(l, l.clone());
    assert_eq!(q, q.clone());
    assert_eq!(p, p.clone());
    assert_eq!(v, v.clone());
    for s in [
        format!("{l:?}"),
        format!("{q:?}"),
        format!("{p:?}"),
        format!("{v:?}"),
    ] {
        assert!(!s.is_empty());
    }
    let session = Session {
        token: "t".into(),
        user: User::default(),
    };
    assert_eq!(session, session.clone());
    assert!(!format!("{session:?}").is_empty());
    assert_eq!(MAX_PER_PAGE, 200);
    assert_eq!(DEFAULT_PER_PAGE, 60);
}
