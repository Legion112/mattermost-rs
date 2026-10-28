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

/// Folding a category sends the **whole** category back, not a patch:
/// the server replaces the one it holds, so a body carrying only
/// `collapsed` would blank the display name and empty the channel list.
#[test]
fn updating_a_category_sends_all_of_it_back() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    let category = mm_types::ChannelCategory {
        id: "channels_u1_t1".into(),
        display_name: "Channels".into(),
        collapsed: true,
        channel_ids: vec![ChannelId::new("c1")],
        ..mm_types::ChannelCategory::default()
    };
    let request = update_category(&user, &team, &category);
    assert_eq!(request.method, Method::Put);
    assert_eq!(
        request.path,
        "/api/v4/users/u1/teams/t1/channels/categories/channels_u1_t1"
    );
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""collapsed":true"#), "{body}");
    assert!(body.contains(r#""display_name":"Channels""#), "{body}");
    assert!(body.contains(r#""channel_ids":["c1"]"#), "{body}");

    // The server answers with the category as it now stands.
    let back = parse_category(&ok(
        r#"{"id":"channels_u1_t1","type":"channels","sorting":"",
            "display_name":"Channels","collapsed":true,"channel_ids":["c1"]}"#,
    ))
    .unwrap();
    assert!(back.collapsed);
}

/// The threads endpoint, and the totals that feed the sidebar badge --
/// which are the account's, not the page's, so asking for one page and
/// counting it would under-report.
#[test]
fn the_threads_endpoint_reports_account_wide_totals() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    let request = my_threads(&user, &team, 25);
    assert_eq!(request.path, "/api/v4/users/u1/teams/t1/threads");
    assert!(
        request
            .query
            .contains(&("per_page".to_owned(), "25".to_owned()))
    );
    // Participants are asked for unextended: the roster already names them.
    assert!(
        request
            .query
            .contains(&("extended".to_owned(), "false".to_owned()))
    );

    let list = parse_threads(&ok(
        r#"{"total":9,"total_unread_threads":2,"total_unread_mentions":1,
            "threads":[{"id":"p1","reply_count":3,"unread_replies":2,
                        "participants":[{"id":"u2"}],
                        "post":{"id":"p1","message":"root"}}]}"#,
    ))
    .unwrap();
    assert_eq!(list.total_unread_threads, 2);
    assert_eq!(list.threads.len(), 1);
    assert_eq!(list.threads[0].unread_replies, 2);
}

/// Drafts are server-side, so this lists what was typed in any client.
#[test]
fn the_drafts_endpoint_lists_unsent_messages() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    assert_eq!(
        my_drafts(&user, &team).path,
        "/api/v4/users/u1/teams/t1/drafts"
    );
    let drafts = parse_drafts(&ok(
        r#"[{"channel_id":"c1","message":"half a thought","update_at":5}]"#,
    ))
    .unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].message, "half a thought");
}

/// An account with no drafts answers `null`, not `[]`. Parsing that
/// into a `Vec` fails, and a swallowed failure looks exactly like an
/// account with no drafts -- so the Drafts row simply stopped appearing
/// and nothing said why.
#[test]
fn no_drafts_comes_back_as_null_not_an_empty_array() {
    assert!(parse_drafts(&ok("null")).unwrap().is_empty());
    assert!(parse_drafts(&ok("[]")).unwrap().is_empty());
    assert_eq!(
        parse_drafts(&ok(r#"[{"channel_id":"c1","message":"x"}]"#))
            .unwrap()
            .len(),
        1
    );
}

/// Editing carries the post's own id in the body as well as the path,
/// which is what the server's model expects: it deserializes a whole
/// Post and checks the two agree.
#[test]
fn editing_a_post_sends_the_id_and_the_new_message() {
    let request = edit_post(&PostId::new("p1"), "a second thought");
    assert_eq!(request.method, Method::Put);
    assert_eq!(request.path, "/api/v4/posts/p1");
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""id":"p1""#), "{body}");
    assert!(body.contains(r#""message":"a second thought""#), "{body}");
}

/// Who is in a channel, which is who `@` may mean while reading it.
/// Asked once when the channel opens, not per keystroke: the answer is
/// the same for every letter typed.
#[test]
fn users_in_channel_is_one_page_per_channel() {
    let request = users_in_channel(&ChannelId::new("c1"), 200);
    assert_eq!(request.path, "/api/v4/users");
    assert!(
        request
            .query
            .contains(&("in_channel".to_owned(), "c1".to_owned()))
    );
    assert!(
        request
            .query
            .contains(&("per_page".to_owned(), "200".to_owned()))
    );
    let users = parse_users_in_channel(&ok(
        r#"[{"id":"u1","username":"presenceaura","first_name":"Roman"}]"#,
    ))
    .unwrap();
    assert_eq!(users[0].username, "presenceaura");
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

    let page = posts_for_channel(&c, &PostQuery::first(), false);
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
        false,
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
        false,
    );
    assert_eq!(
        after.query,
        vec![
            ("after".to_owned(), "p1".to_owned()),
            ("per_page".to_owned(), "30".to_owned())
        ]
    );

    let since = posts_for_channel(&c, &PostQuery::Since { millis: 1_700 }, false);
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
        false,
    );
    assert_eq!(huge.query[1], ("per_page".to_owned(), "200".to_owned()));

    let zero = posts_for_channel(
        &c,
        &PostQuery::Page {
            page: 0,
            per_page: 0,
        },
        false,
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
    let root = create_post(&c, "hello", None, "pending-1", &[]);
    let Body::Json(body) = &root.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""pending_post_id":"pending-1""#), "{body}");
    assert!(
        !body.contains("root_id"),
        "a root post sends no root_id: {body}"
    );

    let reply = create_post(&c, "hello", Some("p1"), "pending-2", &[]);
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

/// Reacting and un-reacting are different shapes: one posts a body, the
/// other puts the emoji in the path.
#[test]
fn reactions_are_added_by_body_and_removed_by_path() {
    let user = UserId::new("u1");
    let post = PostId::new("p1");

    let add = add_reaction(&user, &post, "tada");
    assert_eq!(add.path, "/api/v4/reactions");
    assert_eq!(add.method, Method::Post);
    let Body::Json(body) = &add.body else {
        panic!("expected a JSON body");
    };
    let body: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(body["user_id"], "u1");
    assert_eq!(body["post_id"], "p1");
    assert_eq!(body["emoji_name"], "tada");

    let remove = remove_reaction(&user, &post, "tada");
    assert_eq!(remove.method, Method::Delete);
    assert_eq!(remove.path, "/api/v4/users/u1/posts/p1/reactions/tada");

    let parsed = parse_reaction(&ok(r#"{"emoji_name":"tada","user_id":"u1"}"#)).unwrap();
    assert_eq!(parsed.emoji_name, "tada");
}

/// Avatars come back as bytes, not JSON, but a failure still comes back as a
/// JSON error document and has to be read as one.
#[test]
fn a_profile_image_is_bytes_on_success_and_an_api_error_on_failure() {
    let user = UserId::new("u1");
    assert_eq!(profile_image(&user).path, "/api/v4/users/u1/image");

    let png = [0x89, b'P', b'N', b'G', 0x0d];
    assert_eq!(
        parse_image(&HttpResponse::new(200, png.to_vec())).unwrap(),
        png
    );
    let err = HttpResponse::new(404, r#"{"id":"api.user.no","message":"gone"}"#);
    assert!(matches!(parse_image(&err), Err(ApiError::NotFound(_))));
}

/// The preview is a different path from the file itself, and a message list
/// wants the preview: the original can be a twelve-megapixel phone photo
/// going into a four-hundred-pixel row.
#[test]
fn a_file_is_fetched_whole_or_as_the_servers_preview() {
    let f = mm_types::FileId::new("f1");
    assert_eq!(file_contents(&f, false).path, "/api/v4/files/f1");
    assert_eq!(file_contents(&f, true).path, "/api/v4/files/f1/preview");
    assert_eq!(file_contents(&f, true).method, Method::Get);
}

/// The bug this exists for: with collapsed reply threads the server counts
/// replies against the page size, and a client that hides replies is left
/// with whatever roots happen to be in the sixty posts it was sent. One
/// real channel's page of sixty held five roots, so the channel looked
/// nearly empty. `collapsedThreads=true` makes the page sixty roots.
#[test]
fn a_collapsed_page_asks_the_server_for_roots_only() {
    let c = ChannelId::new("c1");

    let collapsed = posts_for_channel(&c, &PostQuery::first(), true);
    assert!(
        collapsed
            .query
            .contains(&("collapsedThreads".to_owned(), "true".to_owned())),
        "{:?}",
        collapsed.query
    );

    // And the flat view must not send it: there the replies belong in the
    // channel and leaving them out would hide messages.
    let flat = posts_for_channel(&c, &PostQuery::first(), false);
    assert!(
        !flat.query.iter().any(|(k, _)| k == "collapsedThreads"),
        "{:?}",
        flat.query
    );

    // It rides alongside the paging, not instead of it.
    let older = posts_for_channel(
        &c,
        &PostQuery::Before {
            post: "p1".into(),
            per_page: 60,
        },
        true,
    );
    assert!(older.query.iter().any(|(k, _)| k == "collapsedThreads"));
    assert!(older.query.iter().any(|(k, v)| k == "before" && v == "p1"));
}

/// Pinning is two paths rather than a body flag, which is how the server
/// models it.
#[test]
fn pinning_and_unpinning_are_different_paths() {
    let p = PostId::new("p1");
    assert_eq!(set_pinned(&p, true).path, "/api/v4/posts/p1/pin");
    assert_eq!(set_pinned(&p, false).path, "/api/v4/posts/p1/unpin");
    assert_eq!(set_pinned(&p, true).method, Method::Post);
}

/// Deleting is a DELETE on the post itself; the removal reaches the client
/// as a `post_deleted` event, not as this response.
#[test]
fn deleting_a_post_is_a_delete_on_the_post() {
    let request = delete_post(&PostId::new("p1"));
    assert_eq!(request.method, Method::Delete);
    assert_eq!(request.path, "/api/v4/posts/p1");
}

/// `set_unread` takes a **native** JSON boolean, unlike most of this
/// API's booleans, which are strings -- and the flag has to match what
/// the client does or the server maintains the wrong unread counters.
#[test]
fn marking_unread_sends_a_real_boolean() {
    let request = set_unread(&UserId::new("u1"), &PostId::new("p1"), true);
    assert_eq!(request.path, "/api/v4/users/u1/posts/p1/set_unread");
    let Body::Json(body) = &request.body else {
        panic!("expected a JSON body");
    };
    let body: serde_json::Value = serde_json::from_str(body).expect("parses");
    assert_eq!(
        body["collapsed_threads_supported"],
        serde_json::Value::Bool(true),
        "a string would be silently ignored"
    );
}

/// Saving a post is a *preference*, not an endpoint of its own: the
/// `flagged_post` category keyed by post id. Unsaving deletes that row,
/// which is a different path.
#[test]
fn saving_a_post_writes_a_preference() {
    let save = set_saved(&UserId::new("u1"), &PostId::new("p1"), true);
    assert_eq!(save.method, Method::Put);
    assert_eq!(save.path, "/api/v4/users/u1/preferences");
    let Body::Json(body) = &save.body else {
        panic!("expected a JSON body");
    };
    let body: serde_json::Value = serde_json::from_str(body).expect("parses");
    assert_eq!(body[0]["category"], "flagged_post");
    assert_eq!(body[0]["name"], "p1");

    // Not a PUT. Deleting a preference takes a body, so Mattermost
    // models it as an action rather than a DELETE on a resource -- and
    // a PUT to this path answers 404, which reads like a missing post
    // rather than a wrong verb and leaves the post saved.
    let unsave = set_saved(&UserId::new("u1"), &PostId::new("p1"), false);
    assert_eq!(unsave.path, "/api/v4/users/u1/preferences/delete");
    assert_eq!(unsave.method, Method::Post);
    assert_eq!(unsave.body, save.body);
}

/// Presence for a set of people goes in one POST with the ids in the
/// body, not a query string: a channel roster is two hundred ids, which
/// is past the usual proxy limit and would put everyone the reader is
/// looking at into an access log.
#[test]
fn statuses_for_sends_the_ids_in_the_body() {
    let request = statuses_for(&[UserId::new("u1"), UserId::new("u2")]);
    assert_eq!(request.path, "/api/v4/users/status/ids");
    assert_eq!(request.method, Method::Post);
    assert!(request.query.is_empty());
    match &request.body {
        crate::http::Body::Json(json) => assert_eq!(json, r#"["u1","u2"]"#),
        other => panic!("expected a JSON body, got {other:?}"),
    }
    // Asking about nobody is still well-formed, which is what keeps the
    // caller from needing a special case for an empty channel.
    match &statuses_for(&[]).body {
        crate::http::Body::Json(json) => assert_eq!(json, "[]"),
        other => panic!("expected a JSON body, got {other:?}"),
    }

    let statuses = parse_statuses(&ok(
        r#"[{"user_id":"u1","status":"online","manual":false,"last_activity_at":17}]"#,
    ))
    .unwrap();
    assert_eq!(statuses[0].presence(), mm_types::Presence::Online);
    assert!(parse_statuses(&ok("not json")).is_err());
}

/// Moving a channel between categories sends both of them in one
/// request. Two requests would leave a window in which the server
/// believes the channel is in both categories, or in neither, and the
/// sidebar that comes back from the next sync would say so.
#[test]
fn moving_a_channel_updates_both_categories_at_once() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    let category = |id: &str, channels: &[&str]| mm_types::ChannelCategory {
        id: id.into(),
        display_name: id.into(),
        channel_ids: channels.iter().map(|c| ChannelId::new(*c)).collect(),
        ..mm_types::ChannelCategory::default()
    };
    let request = update_categories(
        &user,
        &team,
        &[category("favorites_u1_t1", &["c1"]), category("qa", &[])],
    );
    assert_eq!(request.method, Method::Put);
    assert_eq!(
        request.path,
        "/api/v4/users/u1/teams/t1/channels/categories"
    );
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    // An array, not an object: the endpoint takes the whole set.
    assert!(body.starts_with('['), "{body}");
    assert!(body.contains(r#""channel_ids":["c1"]"#), "{body}");
    assert!(body.contains(r#""channel_ids":[]"#), "{body}");

    let back = parse_updated_categories(&ok(
        r#"[{"id":"favorites_u1_t1","type":"favorites","sorting":"",
             "display_name":"Favorites","collapsed":false,"channel_ids":["c1"]}]"#,
    ))
    .unwrap();
    assert_eq!(back[0].channel_ids, [ChannelId::new("c1")]);
    assert!(parse_updated_categories(&ok("not json")).is_err());
}

/// Search terms go to the server untouched. Its own query language --
/// `from:`, `in:`, quoted phrases, `-excluded` -- is parsed there, and a
/// client that tried to interpret it could only get it wrong.
#[test]
fn searching_sends_the_terms_as_typed() {
    let request = search_posts(&TeamId::new("t1"), r#"from:legion "deploy report""#, 0, 20);
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path, "/api/v4/teams/t1/posts/search");
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#"from:legion \"deploy report\""#), "{body}");
    // Every term must appear, which is what two typed words mean.
    assert!(body.contains(r#""is_or_search":false"#), "{body}");

    let found = parse_search(&ok(
        r#"{"order":["p1"],"posts":{"p1":{"id":"p1","message":"deploy report"}}}"#,
    ))
    .unwrap();
    assert_eq!(found.order, [PostId::new("p1")]);
    assert!(parse_search(&ok("not json")).is_err());

    // Zero-based, and the only way past the first page: search has no
    // before/after cursor the way a channel's history does.
    let second = search_posts(&TeamId::new("t1"), "deploy", 1, 20);
    let Body::Json(body) = &second.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""page":1"#), "{body}");
    assert!(body.contains(r#""per_page":20"#), "{body}");
}

/// Pinned posts are per channel; saved posts are per reader, and the
/// server still calls them flagged on the wire.
#[test]
fn pinned_and_saved_posts_come_from_their_own_endpoints() {
    assert_eq!(
        pinned_posts(&ChannelId::new("c1")).path,
        "/api/v4/channels/c1/pinned"
    );
    let request = saved_posts(&UserId::new("u1"), 60);
    assert_eq!(request.path, "/api/v4/users/u1/posts/flagged");
    assert!(
        request
            .query
            .contains(&("per_page".to_owned(), "60".to_owned()))
    );

    let page = r#"{"order":["p1"],"posts":{"p1":{"id":"p1","message":"hi"}}}"#;
    assert_eq!(parse_pinned(&ok(page)).unwrap().order.len(), 1);
    assert_eq!(parse_saved(&ok(page)).unwrap().order.len(), 1);
    assert!(parse_pinned(&ok("nope")).is_err());
    assert!(parse_saved(&ok("nope")).is_err());
}

/// A file goes up before the post that carries it, because a post
/// refers to file *ids* and the ids do not exist until the bytes are on
/// the server. The channel goes in the form, not the path.
#[test]
fn a_file_is_uploaded_before_the_post_that_names_it() {
    let c = ChannelId::new("c1");
    let request = upload_file(&c, "shot.png", vec![1, 2, 3]);
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path, "/api/v4/files");
    match &request.body {
        Body::Multipart {
            fields,
            file_field,
            filename,
            content,
        } => {
            assert_eq!(fields, &[("channel_id".to_owned(), "c1".to_owned())]);
            assert_eq!(file_field, "files");
            assert_eq!(filename, "shot.png");
            assert_eq!(content, &[1, 2, 3]);
        }
        other => panic!("expected a multipart body, got {other:?}"),
    }

    let up = parse_upload(&ok(
        r#"{"file_infos":[{"id":"f1","name":"shot.png","size":3}]}"#,
    ))
    .unwrap();
    assert_eq!(up.file_infos[0].id, FileId::new("f1"));
    assert!(parse_upload(&ok("not json")).is_err());

    // And then the post names it. An empty list sends no `file_ids` at
    // all, which is what the web app does.
    let with = create_post(&c, "", None, "pending-1", &[FileId::new("f1")]);
    let Body::Json(body) = &with.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""file_ids":["f1"]"#), "{body}");
    let without = create_post(&c, "hi", None, "pending-2", &[]);
    let Body::Json(body) = &without.body else {
        panic!("expected JSON")
    };
    assert!(!body.contains("file_ids"), "{body}");
}

/// There is no mute flag: `mark_unread` is `"mention"` for a muted
/// channel and `"all"` for one that is not. The endpoint patches, so
/// the reader's other notification props are left alone.
#[test]
fn muting_sets_mark_unread_and_nothing_else() {
    let c = ChannelId::new("c1");
    let u = UserId::new("u1");
    let on = set_channel_muted(&c, &u, true);
    assert_eq!(on.method, Method::Put);
    assert_eq!(on.path, "/api/v4/channels/c1/members/u1/notify_props");
    let Body::Json(body) = &on.body else {
        panic!("expected JSON")
    };
    assert_eq!(body, r#"{"mark_unread":"mention"}"#);

    let Body::Json(off) = &set_channel_muted(&c, &u, false).body else {
        panic!("expected JSON")
    };
    assert_eq!(off, r#"{"mark_unread":"all"}"#);
}

/// The quick switcher can only offer what is in the sidebar, so a
/// channel the reader has not joined is unreachable without these.
#[test]
fn channels_and_people_can_be_searched_on_the_server() {
    let team = TeamId::new("t1");
    let request = search_channels(&team, "devops");
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path, "/api/v4/teams/t1/channels/search");
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    assert_eq!(body, r#"{"term":"devops"}"#);
    let Body::Json(archived) = &search_archived_channels(&team, "devops").body else {
        panic!("expected JSON")
    };
    assert_eq!(archived, r#"{"deleted":true,"term":"devops"}"#);
    let found =
        parse_channel_search(&ok(r#"[{"id":"c1","display_name":"DevOps","type":"O"}]"#)).unwrap();
    assert_eq!(found[0].display_name, "DevOps");
    assert!(parse_channel_search(&ok("nope")).is_err());

    let people = search_users(&team, "ara", 20);
    assert_eq!(people.path, "/api/v4/users/search");
    let Body::Json(body) = &people.body else {
        panic!("expected JSON")
    };
    // Somebody who has left cannot be messaged, so they are not offered.
    assert!(body.contains(r#""allow_inactive":false"#), "{body}");
    assert!(body.contains(r#""team_id":"t1""#), "{body}");
    assert_eq!(
        parse_user_search(&ok(r#"[{"id":"u1","username":"aragorn"}]"#)).unwrap()[0].username,
        "aragorn"
    );
    assert!(parse_user_search(&ok("nope")).is_err());
}

/// Joining names the person in the body; opening a DM is a bare array
/// of both ids, including your own, and is the same call whether or not
/// the conversation already exists.
#[test]
fn joining_and_opening_a_dm() {
    let me = UserId::new("u-me");
    let join = join_channel(&ChannelId::new("c1"), &me);
    assert_eq!(join.method, Method::Post);
    assert_eq!(join.path, "/api/v4/channels/c1/members");
    let Body::Json(body) = &join.body else {
        panic!("expected JSON")
    };
    assert_eq!(body, r#"{"user_id":"u-me"}"#);

    // Leaving is removing yourself: the same route, and whose id is in the
    // path is the only thing that says which of the two it is.
    let leave = remove_from_channel(&ChannelId::new("c1"), &me);
    assert_eq!(leave.method, Method::Delete);
    assert_eq!(leave.path, "/api/v4/channels/c1/members/u-me");
    assert_eq!(leave.body, Body::Empty);
    assert_eq!(
        remove_from_channel(&ChannelId::new("c1"), &UserId::new("u2")).path,
        "/api/v4/channels/c1/members/u2"
    );

    let dm = open_direct(&me, &UserId::new("u2"));
    assert_eq!(dm.path, "/api/v4/channels/direct");
    let Body::Json(body) = &dm.body else {
        panic!("expected JSON")
    };
    assert_eq!(body, r#"["u-me","u2"]"#);
    assert_eq!(
        parse_channel(&ok(r#"{"id":"c9","type":"D","name":"u-me__u2"}"#))
            .unwrap()
            .id,
        ChannelId::new("c9")
    );
    assert!(parse_channel(&ok("nope")).is_err());
}

/// Joining answers with the new membership, and the channel itself has
/// to be fetched: one found by search was never in this client's own
/// list, so there is nothing local to show the reader after they join.
#[test]
fn joining_answers_with_a_membership_and_the_channel_is_fetched_separately() {
    let member = parse_channel_member(&ok(
        r#"{"channel_id":"c1","user_id":"u1","notify_props":{},"last_viewed_at":0}"#,
    ))
    .unwrap();
    assert_eq!(member.channel_id, ChannelId::new("c1"));
    assert!(parse_channel_member(&ok("nope")).is_err());

    let request = one_channel(&ChannelId::new("c1"));
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path, "/api/v4/channels/c1");
    assert!(request.query.is_empty());
}

/// A permalink carries a post and nothing else; which channel it is in
/// is a thing only the post knows, so following one starts here.
#[test]
fn one_post_is_what_a_permalink_resolves_to() {
    let request = one_post(&PostId::new("p1"));
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path, "/api/v4/posts/p1");
    let post = parse_post(&ok(r#"{"id":"p1","channel_id":"c1","message":"hi"}"#)).unwrap();
    assert_eq!(post.channel_id, ChannelId::new("c1"));
}

/// The thread's id is the root post's id, and the verb carries the
/// whole meaning: there is no `following` field on what the server
/// returns, because being in the followed list is what following is.
#[test]
fn following_a_thread_is_a_put_and_unfollowing_is_a_delete() {
    let user = UserId::new("u1");
    let team = TeamId::new("t1");
    let root = PostId::new("p1");

    let on = follow_thread(&user, &team, &root, true);
    assert_eq!(on.method, Method::Put);
    assert_eq!(on.path, "/api/v4/users/u1/teams/t1/threads/p1/following");
    assert!(matches!(on.body, Body::Empty), "no body either way");

    let off = follow_thread(&user, &team, &root, false);
    assert_eq!(off.method, Method::Delete);
    assert_eq!(off.path, on.path);
    assert!(matches!(off.body, Body::Empty));
}

/// Sorted by name, and asked for by page. The default order is creation
/// time, which changes as people upload -- so paging through it shows
/// the same emoji twice and misses another.
#[test]
fn custom_emoji_is_listed_by_name_a_page_at_a_time() {
    let first = custom_emoji(0, 200);
    assert_eq!(first.method, Method::Get);
    assert!(first.path.starts_with("/api/v4/emoji?"), "{}", first.path);
    assert!(first.path.contains("page=0"), "{}", first.path);
    assert!(first.path.contains("per_page=200"), "{}", first.path);
    assert!(first.path.contains("sort=name"), "{}", first.path);

    assert!(custom_emoji(3, 200).path.contains("page=3"));
}

/// By id, not by name: a name is user-supplied text going into a URL
/// path, and the list this client already holds maps one to the other.
#[test]
fn an_emojis_image_is_fetched_by_id() {
    let image = custom_emoji_image(&mm_types::EmojiId::new("e1"));
    assert_eq!(image.path, "/api/v4/emoji/e1/image");
    assert_eq!(image.method, Method::Get);
}

/// `null` on an empty page, like the drafts endpoint. Parsing straight
/// into a `Vec` would make a server with no custom emoji at all look
/// like a failed request.
#[test]
fn an_empty_emoji_page_is_null_not_an_array() {
    assert!(parse_custom_emoji(&ok("null")).unwrap().is_empty());
    assert!(parse_custom_emoji(&ok("[]")).unwrap().is_empty());

    let listed = parse_custom_emoji(&ok(
        r#"[{"id":"e1","name":"party_parrot","creator_id":"u1","create_at":1,"delete_at":0}]"#,
    ))
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "party_parrot");

    let err = HttpResponse::new(403, r#"{"id":"api.no","message":"nope"}"#);
    assert!(matches!(
        parse_custom_emoji(&err),
        Err(ApiError::Forbidden(_))
    ));
}

/// The body repeats the id the path already carries, which is what the
/// server wants rather than a redundancy worth trimming -- it rejects a
/// body without it. And `PUT`, which is what marks the status manual so
/// the server stops moving it on its own.
#[test]
fn saying_where_you_are_repeats_your_id_in_the_body() {
    let me = UserId::new("u1");
    let req = set_status(&me, mm_types::Presence::Dnd, None);
    assert_eq!(req.method, Method::Put);
    assert_eq!(req.path, "/api/v4/users/u1/status");
    let Body::Json(body) = &req.body else {
        panic!("expected a JSON body, got {:?}", req.body);
    };
    assert!(body.contains(r#""user_id":"u1""#), "{body}");
    assert!(body.contains(r#""status":"dnd""#), "{body}");

    let back = parse_status(&ok(r#"{"user_id":"u1","status":"dnd","manual":true}"#)).unwrap();
    assert_eq!(back.presence(), mm_types::Presence::Dnd);
    assert!(back.manual);

    let err = HttpResponse::new(403, r#"{"id":"api.no","message":"nope"}"#);
    assert!(matches!(parse_status(&err), Err(ApiError::Forbidden(_))));
}

/// `POST /drafts`, with no user or channel in the path: the body
/// carries both. Measured, because the obvious shapes all answer 404 --
/// `/users/{id}/drafts`, `/users/{id}/channels/{id}/drafts` and
/// `/users/{id}/teams/{id}/drafts`.
#[test]
fn a_draft_is_written_to_a_path_with_nothing_in_it() {
    let draft = mm_types::Draft {
        channel_id: ChannelId::new("c1"),
        message: "half a thought".to_owned(),
        ..mm_types::Draft::default()
    };
    let request = upsert_draft(&draft);
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path, "/api/v4/drafts");
    let Body::Json(body) = &request.body else {
        panic!("expected JSON")
    };
    assert!(body.contains(r#""channel_id":"c1""#), "{body}");
    assert!(body.contains("half a thought"), "{body}");
}

/// By channel, and by root when the draft is a reply: a channel can
/// hold one of each at once and deleting the channel's must not take
/// the thread's with it.
#[test]
fn a_draft_is_deleted_by_channel_and_by_root() {
    let user = UserId::new("u1");
    let channel = ChannelId::new("c1");
    let whole = delete_draft(&user, &channel, None);
    assert_eq!(whole.method, Method::Delete);
    assert_eq!(whole.path, "/api/v4/users/u1/channels/c1/drafts");

    let reply = delete_draft(&user, &channel, Some(&PostId::new("p9")));
    assert_eq!(reply.path, "/api/v4/users/u1/channels/c1/drafts?root_id=p9");
}

/// `in_team` is required whenever `in_channel` is given: without it
/// the server answers **500**, not 400, with "Team id parameter is
/// required to autocomplete by channel" -- which reads like a server
/// fault rather than a missing parameter.
#[test]
fn autocompleting_a_mention_names_the_team_as_well_as_the_channel() {
    let request = autocomplete_users(&TeamId::new("t1"), &ChannelId::new("c1"), "al", 8);
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path, "/api/v4/users/autocomplete");
    assert_eq!(
        request.route_key(),
        "GET /api/v4/users/autocomplete?in_team=t1&in_channel=c1&name=al&limit=8"
    );

    // Two lists, and the in-channel one is called `users`.
    let found = parse_autocomplete(&ok(r#"{"users":[{"id":"u1","username":"alukard"}],
            "out_of_channel":[{"id":"u2","username":"alextim"}]}"#))
    .unwrap();
    assert_eq!(found.users[0].username, "alukard");
    assert_eq!(found.out_of_channel[0].username, "alextim");

    // A server that sends neither list is a server with nobody to
    // offer, not a parse failure.
    let empty = parse_autocomplete(&ok("{}")).unwrap();
    assert!(empty.users.is_empty() && empty.out_of_channel.is_empty());
    assert!(!format!("{empty:?}").is_empty());
}

/// The moment goes in the **path**, not a body or a query, and it is
/// the moment to mark read *up to* -- so the last reply's own
/// `create_at` can leave that reply unread, which is why the webapp
/// sends one millisecond past it.
#[test]
fn a_thread_is_marked_read_up_to_a_moment_in_the_path() {
    let request = read_thread(
        &UserId::new("u1"),
        &TeamId::new("t1"),
        &PostId::new("p9"),
        mm_types::Timestamp(1_793_000_000_001),
    );
    assert_eq!(request.method, Method::Put);
    assert_eq!(
        request.path,
        "/api/v4/users/u1/teams/t1/threads/p9/read/1793000000001"
    );
    assert!(request.query.is_empty());
    assert_eq!(request.body, Body::Empty);

    let all = read_all_threads(&UserId::new("u1"), &TeamId::new("t1"));
    assert_eq!(all.method, Method::Put);
    assert_eq!(all.path, "/api/v4/users/u1/teams/t1/threads/read");
}

/// The same patching endpoint as muting, and deliberately a separate
/// call: `mark_unread` is about the bold in the sidebar and `desktop`
/// is about the banner, and one request carrying both would mean
/// every change to one restating the other.
#[test]
fn a_channels_notification_level_patches_only_that_prop() {
    let request = set_channel_notify(&ChannelId::new("c1"), &UserId::new("u1"), "mention");
    assert_eq!(request.method, Method::Put);
    assert_eq!(request.path, "/api/v4/channels/c1/members/u1/notify_props");
    assert_eq!(
        request.body,
        Body::Json(r#"{"desktop":"mention"}"#.to_owned())
    );
    // Muting still says only what muting means.
    assert_eq!(
        set_channel_muted(&ChannelId::new("c1"), &UserId::new("u1"), true).body,
        Body::Json(r#"{"mark_unread":"mention"}"#.to_owned())
    );
}

#[test]
fn a_custom_status_is_put_on_me_and_an_empty_expiry_is_left_out() {
    let forever = mm_types::CustomStatus {
        emoji: "palm_tree".to_owned(),
        text: "On a vacation".to_owned(),
        expires_at: String::new(),
    };
    let req = set_custom_status(&forever, "");
    assert_eq!(req.method, Method::Put);
    assert_eq!(req.path, "/api/v4/users/me/status/custom");
    let Body::Json(body) = &req.body else {
        panic!("expected a JSON body, got {:?}", req.body);
    };
    assert!(body.contains(r#""emoji":"palm_tree""#), "{body}");
    assert!(body.contains(r#""text":"On a vacation""#), "{body}");
    assert!(body.contains(r#""duration":"""#), "{body}");
    assert!(!body.contains("expires_at"), "{body}");

    let today = mm_types::CustomStatus {
        expires_at: "2026-10-28T20:59:59Z".to_owned(),
        ..forever
    };
    let Body::Json(body) = &set_custom_status(&today, "today").body else {
        panic!("expected a JSON body");
    };
    assert!(body.contains(r#""duration":"today""#), "{body}");
    assert!(
        body.contains(r#""expires_at":"2026-10-28T20:59:59Z""#),
        "{body}"
    );

    let back = parse_custom_status(&ok(
        r#"{"emoji":"palm_tree","text":"On a vacation","duration":"today","expires_at":"2026-10-28T20:59:59Z"}"#,
    ))
    .unwrap();
    assert_eq!(back.text, "On a vacation");
    assert_eq!(back.expires_at, "2026-10-28T20:59:59Z");
    let err = HttpResponse::new(400, r#"{"id":"api.bad","message":"too long"}"#);
    assert!(parse_custom_status(&err).is_err());

    let gone = clear_custom_status();
    assert_eq!(gone.method, Method::Delete);
    assert_eq!(gone.path, "/api/v4/users/me/status/custom");
}

/// A timed do-not-disturb says when it ends, in seconds; any other
/// status leaves the field out, and so does an untimed one.
#[test]
fn a_timed_quiet_carries_its_end_in_seconds() {
    let me = UserId::new("u1");
    let Body::Json(body) = &set_status(&me, mm_types::Presence::Dnd, Some(1_730_000_000)).body
    else {
        panic!("expected a JSON body");
    };
    assert!(body.contains(r#""dnd_end_time":1730000000"#), "{body}");
    let Body::Json(body) = &set_status(&me, mm_types::Presence::Online, Some(1_730_000_000)).body
    else {
        panic!("expected a JSON body");
    };
    assert!(!body.contains("dnd_end_time"), "{body}");
    let back = parse_status(&ok(
        r#"{"user_id":"u1","status":"dnd","manual":true,"dnd_end_time":1730000000}"#,
    ))
    .unwrap();
    assert_eq!(back.dnd_end_time, 1_730_000_000);
    let back = parse_status(&ok(r#"{"user_id":"u1","status":"online","manual":false}"#)).unwrap();
    assert_eq!(back.dnd_end_time, 0);
}

/// A big file goes up as a session: opened with its size, fed in pieces,
/// and named by the server only when the last piece is in.
#[test]
fn an_upload_session_is_opened_fed_and_finished() {
    let c = ChannelId::new("c1");
    let open = create_upload(&c, "cast.webm", 5_000_000);
    assert_eq!(open.method, Method::Post);
    assert_eq!(open.path, "/api/v4/uploads");
    let Body::Json(body) = &open.body else {
        panic!("expected a JSON body, got {:?}", open.body);
    };
    assert!(body.contains(r#""file_size":5000000"#), "{body}");
    assert!(body.contains(r#""filename":"cast.webm""#), "{body}");
    let session = parse_upload_session(&ok(
        r#"{"id":"up1","type":"attachment","file_size":5000000,"file_offset":0}"#,
    ))
    .unwrap();
    assert_eq!(session.id, "up1");
    assert_eq!(session.file_size, 5_000_000);

    let piece = upload_chunk("up1", vec![1, 2, 3]);
    assert_eq!(piece.path, "/api/v4/uploads/up1");
    assert!(matches!(&piece.body, Body::Bytes { content_type, content }
        if content_type == "application/octet-stream" && content == &[1, 2, 3]));

    // Short of the size: nothing yet, either way the server says it.
    assert_eq!(parse_chunk(&HttpResponse::new(204, "")).unwrap(), None);
    assert_eq!(parse_chunk(&HttpResponse::new(200, "")).unwrap(), None);
    // The last piece: the file.
    let done = parse_chunk(&ok(r#"{"id":"f1","name":"cast.webm","size":5000000}"#)).unwrap();
    assert_eq!(done.map(|f| f.id.to_string()), Some("f1".to_owned()));
    let err = HttpResponse::new(413, r#"{"id":"api.too_large","message":"too large"}"#);
    assert!(parse_chunk(&err).is_err());
}

/// A draft names its files by id; the record behind one is a request away.
#[test]
fn a_files_record_is_fetched_by_id() {
    let req = file_info(&mm_types::FileId::new("f1"));
    assert_eq!(req.method, Method::Get);
    assert_eq!(req.path, "/api/v4/files/f1/info");
    let info = parse_file_info(&ok(r#"{"id":"f1","name":"notes.txt","size":12}"#)).unwrap();
    assert_eq!(info.name, "notes.txt");
    assert!(parse_file_info(&HttpResponse::new(404, r#"{"id":"x","message":"gone"}"#)).is_err());
}

/// A channel is renamed or re-headed with only what changed, and
/// archived with a DELETE that keeps everything.
#[test]
fn a_channel_is_patched_with_what_changed_and_archived_with_a_delete() {
    let c = ChannelId::new("c1");
    let req = patch_channel(&c, None, Some("Mockups here"), None);
    assert_eq!(req.method, Method::Put);
    assert_eq!(req.path, "/api/v4/channels/c1/patch");
    assert_eq!(
        req.body,
        Body::Json(r#"{"header":"Mockups here"}"#.to_owned())
    );
    let req = patch_channel(&c, Some("Design"), Some(""), Some("Pictures"));
    assert_eq!(
        req.body,
        Body::Json(r#"{"display_name":"Design","header":"","purpose":"Pictures"}"#.to_owned())
    );
    let gone = archive_channel(&c);
    assert_eq!(gone.method, Method::Delete);
    assert_eq!(gone.path, "/api/v4/channels/c1");
}

/// The reader's record is patched with only what changed.
#[test]
fn the_readers_record_is_patched_with_what_changed() {
    let me = UserId::new("u1");
    let req = patch_user(&me, None, None, None, Some("Lab technician"));
    assert_eq!(req.method, Method::Put);
    assert_eq!(req.path, "/api/v4/users/u1/patch");
    assert_eq!(
        req.body,
        Body::Json(r#"{"position":"Lab technician"}"#.to_owned())
    );
    let req = patch_user(&me, Some("Ada"), Some("Lovelace"), Some(""), None);
    assert_eq!(
        req.body,
        Body::Json(r#"{"first_name":"Ada","last_name":"Lovelace","nickname":""}"#.to_owned())
    );
}
