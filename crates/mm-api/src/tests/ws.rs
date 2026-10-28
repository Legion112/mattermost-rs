use super::*;

fn hello_frame(conn: &str, seq: u64) -> String {
    format!(
        r#"{{"event":"hello","data":{{"connection_id":"{conn}",
                "server_version":"10.6.0.13685270376.abc.true"}},"seq":{seq}}}"#
    )
}

fn posted_frame(seq: u64, post_id: &str) -> String {
    let post = format!(r#"{{"id":"{post_id}","message":"hi","channel_id":"c1"}}"#);
    let encoded = serde_json::to_string(&post).unwrap();
    format!(
        r#"{{"event":"posted","data":{{"post":{encoded},
                "channel_display_name":"Town Square","sender_name":"@jane",
                "team_id":"t1"}},"seq":{seq}}}"#
    )
}

/// The server rejects seq <= 0, so the first frame we ever send must be 1.
#[test]
fn client_sequence_starts_at_one_and_increments() {
    let mut p = WsProtocol::new();
    let first = p.auth_frame("tok");
    assert!(first.contains(r#""seq":1"#), "{first}");
    assert!(first.contains(r#""action":"authentication_challenge""#));
    assert!(first.contains(r#""token":"tok""#));

    let second = p.typing_frame(&ChannelId::new("c1"), "");
    assert!(second.contains(r#""seq":2"#), "{second}");
}

/// The server's field is `parent_id`; sending `root_id` means the typing
/// indicator never appears in a thread.
#[test]
fn typing_frame_uses_parent_id() {
    let mut p = WsProtocol::new();
    let frame = p.typing_frame(&ChannelId::new("c1"), "p9");
    assert!(frame.contains(r#""parent_id":"p9""#), "{frame}");
    assert!(!frame.contains("root_id"), "{frame}");
}

/// A token containing a quote must not produce a broken frame.
#[test]
fn frames_escape_their_values() {
    let mut p = WsProtocol::new();
    let frame = p.auth_frame("a\"b");
    let parsed: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(parsed["data"]["token"], "a\"b");
}

#[test]
fn hello_captures_the_connection_id_and_version() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(&hello_frame("conn-1", 0));
    assert_eq!(
        out,
        vec![WsOutcome::Hello {
            connection_id: "conn-1".to_owned(),
            server_version: "10.6.0.13685270376.abc.true".to_owned(),
            lost_events: false,
        }]
    );
    assert_eq!(p.connection_id(), Some("conn-1"));
    assert_eq!(p.server_version(), Some("10.6.0.13685270376.abc.true"));
}

/// The whole point of the hello signal: a *new* connection id while we
/// already held one means the dead queue could not cover the gap.
#[test]
fn a_hello_with_a_new_connection_id_means_events_were_lost() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("conn-1", 0));
    let out = p.on_frame(&hello_frame("conn-2", 0));
    assert_eq!(
        out,
        vec![WsOutcome::Hello {
            connection_id: "conn-2".to_owned(),
            server_version: "10.6.0.13685270376.abc.true".to_owned(),
            lost_events: true,
        }]
    );
}

/// A successful resume replays the same connection id, so nothing was lost.
#[test]
fn a_hello_repeating_the_same_connection_id_is_not_a_loss() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("conn-1", 0));
    let out = p.on_frame(&hello_frame("conn-1", 0));
    assert!(matches!(
        out.first(),
        Some(WsOutcome::Hello {
            lost_events: false,
            ..
        })
    ));
}

#[test]
fn posted_decodes_the_json_encoded_post() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("c", 0));
    let out = p.on_frame(&posted_frame(1, "p1"));
    match &out[0] {
        WsOutcome::Event(WsEvent::Posted {
            post,
            channel_display_name,
            sender_name,
            team_id,
            mentions,
        }) => {
            assert_eq!(post.id.as_str(), "p1");
            assert_eq!(post.message, "hi");
            assert_eq!(channel_display_name, "Town Square");
            assert_eq!(sender_name, "@jane");
            assert_eq!(team_id, "t1");
            assert!(mentions.is_empty());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// A nested object where the protocol sends a JSON-encoded string is the
/// classic mistake, so it must be reported rather than silently defaulted.
#[test]
fn a_nested_post_object_is_reported_as_undecodable() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(r#"{"event":"posted","data":{"post":{"id":"p1"}},"seq":1}"#);
    match &out[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(
                detail.contains("JSON document inside a JSON string"),
                "{detail}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn mentions_are_decoded_when_the_server_includes_them() {
    let post = serde_json::to_string(r#"{"id":"p1","message":"@jane hi"}"#).unwrap();
    let mentions = serde_json::to_string(r#"["u-jane"]"#).unwrap();
    let frame =
        format!(r#"{{"event":"posted","data":{{"post":{post},"mentions":{mentions}}},"seq":1}}"#);
    let mut p = WsProtocol::new();
    match &p.on_frame(&frame)[0] {
        WsOutcome::Event(WsEvent::Posted { mentions, .. }) => {
            assert_eq!(mentions, &vec!["u-jane".to_owned()]);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// A hole in the event sequence means events were dropped, and the
/// consumer must learn that *before* it processes the event that followed
/// the hole.
#[test]
fn a_sequence_hole_is_reported_before_the_event() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("c", 0));
    p.on_frame(&posted_frame(1, "p1"));
    let out = p.on_frame(&posted_frame(5, "p5"));
    assert_eq!(
        out[0],
        WsOutcome::SequenceGap {
            expected: 2,
            got: 5
        },
        "the gap must come first"
    );
    assert!(matches!(out[1], WsOutcome::Event(WsEvent::Posted { .. })));
}

#[test]
fn contiguous_events_report_no_gap() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("c", 0));
    for seq in 1..=4 {
        let out = p.on_frame(&posted_frame(seq, "p"));
        assert_eq!(out.len(), 1, "seq {seq} should not report a gap");
    }
}

#[test]
fn replies_are_matched_to_the_frame_that_caused_them() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(r#"{"status":"OK","seq_reply":1}"#);
    assert_eq!(
        out,
        vec![WsOutcome::Reply {
            client_seq: 1,
            ok: true,
            error: None
        }]
    );

    let out = p.on_frame(r#"{"status":"FAIL","seq_reply":2,"error":{"id":"x","message":"no"}}"#);
    match &out[0] {
        WsOutcome::Reply {
            client_seq,
            ok,
            error,
        } => {
            assert_eq!(*client_seq, 2);
            assert!(!ok);
            assert_eq!(error.as_ref().unwrap().id, "x");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn channel_member_updated_decodes_the_camel_cased_json_string() {
    let member = serde_json::to_string(
        r#"{"channel_id":"c1","msg_count":7,"mention_count":1,
                "notify_props":{"mark_unread":"mention"}}"#,
    )
    .unwrap();
    let frame =
        format!(r#"{{"event":"channel_member_updated","data":{{"channelMember":{member}}}}}"#);
    let mut p = WsProtocol::new();
    match &p.on_frame(&frame)[0] {
        WsOutcome::Event(WsEvent::ChannelMemberUpdated(m)) => {
            assert_eq!(m.msg_count, 7);
            assert_eq!(m.mention_count, 1);
            assert!(m.is_muted());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// `channel_times` is a nested map, NOT a JSON-encoded string — one of the
/// two exceptions to the rule.
#[test]
fn multiple_channels_viewed_reads_a_nested_map() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(
        r#"{"event":"multiple_channels_viewed","data":{"channel_times":{"c1":1700,"c2":1800}}}"#,
    );
    match &out[0] {
        WsOutcome::Event(WsEvent::MultipleChannelsViewed { channel_times }) => {
            assert_eq!(channel_times["c1"], 1700);
            assert_eq!(channel_times["c2"], 1800);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// `channelMember` is the authoritative read-state push, so a payload
/// this client cannot decode has to be reported rather than silently
/// becoming a default membership with zero unreads.
#[test]
fn a_bad_channel_member_payload_is_reported() {
    let mut p = WsProtocol::new();
    match &p.on_frame(r#"{"event":"channel_member_updated","data":{"channelMember":"{bad}"}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(detail.contains("channelMember"), "{detail}");
        }
        other => panic!("unexpected: {other:?}"),
    }

    let mut p2 = WsProtocol::new();
    match &p2.on_frame(r#"{"event":"channel_member_updated","data":{}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(detail.contains("missing"), "{detail}");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// The legacy name still has to work for servers at 8.1 and older.
#[test]
fn the_legacy_channel_viewed_event_is_still_handled() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(r#"{"event":"channel_viewed","data":{"channel_id":"c1"}}"#);
    assert_eq!(
        out[0],
        WsOutcome::Event(WsEvent::ChannelViewed {
            channel_id: ChannelId::new("c1")
        })
    );
}

/// `user` is the other exception: a nested object.
#[test]
fn user_updated_reads_a_nested_object() {
    let mut p = WsProtocol::new();
    let out =
        p.on_frame(r#"{"event":"user_updated","data":{"user":{"id":"u1","username":"jane"}}}"#);
    match &out[0] {
        WsOutcome::Event(WsEvent::UserUpdated(u)) => assert_eq!(u.username, "jane"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn typing_and_status_change_decode() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(
        r#"{"event":"typing","data":{"channel_id":"c1","user_id":"u1","parent_id":"p1"}}"#,
    );
    assert_eq!(
        out[0],
        WsOutcome::Event(WsEvent::Typing {
            channel_id: ChannelId::new("c1"),
            user_id: "u1".to_owned(),
            parent_id: "p1".to_owned()
        })
    );

    let out = p.on_frame(r#"{"event":"status_change","data":{"user_id":"u1","status":"online"}}"#);
    assert_eq!(
        out[0],
        WsOutcome::Event(WsEvent::StatusChange {
            user_id: "u1".to_owned(),
            status: "online".to_owned()
        })
    );
}

/// The server defines over a hundred event types and keeps adding them. An
/// unrecognised one must be inert, never fatal.
#[test]
fn an_unknown_event_is_inert_rather_than_fatal() {
    let mut p = WsProtocol::new();
    let out = p.on_frame(r#"{"event":"burn_on_read_all_revealed","data":{}}"#);
    assert_eq!(
        out[0],
        WsOutcome::Event(WsEvent::Unknown {
            event: "burn_on_read_all_revealed".to_owned()
        })
    );
}

#[test]
fn undecodable_frames_are_reported_not_dropped() {
    let mut p = WsProtocol::new();
    match &p.on_frame("}{")[0] {
        WsOutcome::Undecodable { detail } => assert!(detail.contains("JSON")),
        other => panic!("unexpected: {other:?}"),
    }
    match &p.on_frame(r#"{"data":{}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(
                detail.contains("neither an event nor a seq_reply"),
                "{detail}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
    let mut p2 = WsProtocol::new();
    match &p2.on_frame(r#"{"event":"post_edited","data":{"post":"{bad}"}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(detail.contains("invalid JSON"), "{detail}");
        }
        other => panic!("unexpected: {other:?}"),
    }
    let mut p3 = WsProtocol::new();
    match &p3.on_frame(r#"{"event":"post_deleted","data":{}}"#)[0] {
        WsOutcome::Undecodable { detail } => assert!(detail.contains("missing"), "{detail}"),
        other => panic!("unexpected: {other:?}"),
    }
    let mut p4 = WsProtocol::new();
    match &p4.on_frame(r#"{"event":"multiple_channels_viewed","data":{"channel_times":5}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(detail.contains("channel_times"), "{detail}");
        }
        other => panic!("unexpected: {other:?}"),
    }
    let mut p5 = WsProtocol::new();
    match &p5.on_frame(r#"{"event":"user_updated","data":{"user":5}}"#)[0] {
        WsOutcome::Undecodable { detail } => {
            assert!(detail.contains("user_updated"), "{detail}");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// Resuming is what avoids a full REST resync, so the query must carry
/// both the connection id and the next expected sequence number.
#[test]
fn reconnect_query_asks_the_server_to_replay() {
    let mut p = WsProtocol::new();
    assert!(
        p.reconnect_query(None).is_empty(),
        "nothing to resume before the first hello"
    );

    p.on_frame(&hello_frame("conn-1", 0));
    p.on_frame(&posted_frame(1, "p1"));
    p.on_frame(&posted_frame(2, "p2"));

    assert_eq!(
        p.reconnect_query(Some(CLOSE_CLIENT_PING_TIMEOUT)),
        vec![
            ("connection_id".to_owned(), "conn-1".to_owned()),
            ("sequence_number".to_owned(), "3".to_owned()),
            ("disconnect_err_code".to_owned(), "4000".to_owned()),
        ]
    );

    p.reset();
    assert!(p.reconnect_query(None).is_empty());
}

#[test]
fn reconnect_query_without_events_asks_from_one() {
    let mut p = WsProtocol::new();
    p.on_frame(r#"{"event":"hello","data":{"connection_id":"c1","server_version":"1"}}"#);
    assert_eq!(
        p.reconnect_query(None),
        vec![
            ("connection_id".to_owned(), "c1".to_owned()),
            ("sequence_number".to_owned(), "1".to_owned()),
        ]
    );
}

/// Without jitter a reconnect loop hammers the server at tens of attempts
/// a second; without a cap it eventually waits for hours.
#[test]
fn backoff_is_bounded_monotonic_and_jittered() {
    for attempt in 0..20 {
        let full = next_delay_secs(attempt, 1.0);
        assert!(full <= 30.0, "attempt {attempt} exceeded the cap: {full}");
        assert!(next_delay_secs(attempt, 0.0) <= full);
    }
    assert!((next_delay_secs(0, 1.0) - 1.0).abs() < f64::EPSILON);
    assert!((next_delay_secs(1, 1.0) - 2.0).abs() < f64::EPSILON);
    assert!((next_delay_secs(2, 1.0) - 4.0).abs() < f64::EPSILON);
    assert!((next_delay_secs(10, 1.0) - 30.0).abs() < f64::EPSILON);

    // The ceiling is non-decreasing until it saturates.
    let mut prev = 0.0;
    for attempt in 0..10 {
        let d = next_delay_secs(attempt, 1.0);
        assert!(d >= prev, "attempt {attempt}: {d} < {prev}");
        prev = d;
    }

    // Out-of-range jitter must be clamped rather than producing a
    // negative or enormous delay.
    assert!(next_delay_secs(3, -5.0) >= 0.0);
    assert!(next_delay_secs(3, 5.0) <= 30.0);
}

#[test]
fn constants_match_the_server_contract() {
    assert_eq!(SERVER_PING_INTERVAL_SECS, 60);
    assert_eq!(PING_WATCHDOG_SECS, 65);
    assert_eq!(CLOSE_CLIENT_PING_TIMEOUT, 4000);
    assert_eq!(CLOSE_SEQUENCE_MISMATCH, 4001);
}

#[test]
fn every_derive_is_exercised() {
    let p = WsProtocol::new();
    assert!(!format!("{p:?}").is_empty());
    let cloned = p.clone();
    assert_eq!(cloned.connection_id(), None);
    let o = WsOutcome::SequenceGap {
        expected: 1,
        got: 2,
    };
    assert_eq!(o, o.clone());
    assert!(!format!("{o:?}").is_empty());
    let e = WsEvent::Unknown {
        event: "x".to_owned(),
    };
    assert_eq!(e, e.clone());
    assert!(!format!("{e:?}").is_empty());
}

/// A reconnect must not look like data loss just because the server
/// restarted its event counter. This is the regression test for the bug
/// the ordering above fixes: gap detection used to run across the hello
/// frame and reported a hole on every reconnect.
#[test]
fn a_reconnect_reports_no_spurious_gap() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("conn-1", 0));
    p.on_frame(&posted_frame(1, "p1"));
    p.on_frame(&posted_frame(2, "p2"));

    // The server mints a new connection and restarts its counter at 0.
    let out = p.on_frame(&hello_frame("conn-2", 0));
    assert_eq!(out.len(), 1, "a hello must not also report a gap: {out:?}");
    assert!(matches!(
        out[0],
        WsOutcome::Hello {
            lost_events: true,
            ..
        }
    ));

    // ...and the first event on the new connection is not a gap either.
    let out = p.on_frame(&posted_frame(1, "p3"));
    assert_eq!(out.len(), 1, "first event after a hello: {out:?}");
    assert!(matches!(out[0], WsOutcome::Event(_)));
}

/// `reaction` is a JSON document inside a JSON string, like `post` and
/// `channel` -- the wart that is the classic Mattermost client bug.
#[test]
fn a_reaction_arrives_as_json_inside_a_string() {
    let mut proto = WsProtocol::default();
    let frame = r#"{"event":"reaction_added","seq":1,"data":{"reaction":
        "{\"user_id\":\"u1\",\"post_id\":\"p1\",\"emoji_name\":\"tada\",\"create_at\":20}"}}"#;
    let outcomes = proto.on_frame(frame);
    let Some(WsOutcome::Event(WsEvent::ReactionAdded(reaction))) = outcomes.first() else {
        panic!("expected a decoded reaction, got {outcomes:?}");
    };
    assert_eq!(reaction.emoji_name, "tada");
    assert_eq!(reaction.post_id, mm_types::PostId::new("p1"));

    let gone = r#"{"event":"reaction_removed","seq":2,"data":{"reaction":
        "{\"user_id\":\"u1\",\"post_id\":\"p1\",\"emoji_name\":\"tada\"}"}}"#;
    assert!(matches!(
        proto.on_frame(gone).first(),
        Some(WsOutcome::Event(WsEvent::ReactionRemoved(_)))
    ));
}

/// A reaction frame whose `reaction` is a nested object rather than a string
/// is the shape a client would assume if it had not read the server source.
/// It must be reported, not silently dropped.
#[test]
fn a_reaction_sent_as_a_nested_object_is_reported_not_swallowed() {
    let mut proto = WsProtocol::default();
    let frame = r#"{"event":"reaction_added","seq":1,
                    "data":{"reaction":{"emoji_name":"tada"}}}"#;
    let outcomes = proto.on_frame(frame);
    assert!(
        matches!(outcomes.first(), Some(WsOutcome::Undecodable { .. })),
        "got {outcomes:?}"
    );
}

/// A newly uploaded custom emoji, nested the same way a reaction is.
///
/// Decoded for one reason: without it the person who has just uploaded
/// one sees their own shortcode as text until the next sync.
#[test]
fn an_uploaded_emoji_arrives_as_json_inside_a_string() {
    let mut proto = WsProtocol::default();
    let frame = r#"{"event":"emoji_added","seq":1,"data":{"emoji":
        "{\"id\":\"e1\",\"name\":\"party_parrot\",\"creator_id\":\"u1\",\"create_at\":20}"}}"#;
    let outcomes = proto.on_frame(frame);
    let Some(WsOutcome::Event(WsEvent::EmojiAdded(emoji))) = outcomes.first() else {
        panic!("expected a decoded emoji, got {outcomes:?}");
    };
    assert_eq!(emoji.name, "party_parrot");
    assert_eq!(emoji.id, mm_types::EmojiId::new("e1"));
}

/// Captured from a lab server at 12.0.1 on 2026-10-28, both directions.
///
/// `user_added` puts the channel in `broadcast` and the person in `data`.
/// `user_removed` does the same for everyone still in the channel — but the
/// copy sent to the person who left carries `data.channel_id` and
/// `broadcast.user_id`, with no `broadcast.channel_id` at all. A decoder
/// reading only one side of the frame misses being removed from a channel,
/// which is a row that stays in the sidebar for good.
#[test]
fn membership_events_read_each_field_from_whichever_side_carries_it() {
    let mut p = WsProtocol::new();
    p.on_frame(&hello_frame("conn-1", 0));

    let added = r#"{"event":"user_added",
        "broadcast":{"channel_id":"c1","user_id":""},
        "data":{"team_id":"t1","user_id":"u-oscar"},"seq":1}"#;
    assert_eq!(
        p.on_frame(added),
        vec![WsOutcome::Event(WsEvent::UserAddedToChannel {
            channel_id: ChannelId::new("c1"),
            user_id: UserId::new("u-oscar"),
        })]
    );

    // What everyone else in the channel sees.
    let removed_for_others = r#"{"event":"user_removed",
        "broadcast":{"channel_id":"c1","user_id":""},
        "data":{"remover_id":"u-me","user_id":"u-oscar"},"seq":2}"#;
    assert_eq!(
        p.on_frame(removed_for_others),
        vec![WsOutcome::Event(WsEvent::UserRemovedFromChannel {
            channel_id: ChannelId::new("c1"),
            user_id: UserId::new("u-oscar"),
        })]
    );

    // What the person removed sees: the other way round, in both fields.
    let removed_for_them = r#"{"event":"user_removed",
        "broadcast":{"channel_id":"","user_id":"u-oscar"},
        "data":{"channel_id":"c1","remover_id":"u-me"},"seq":3}"#;
    assert_eq!(
        p.on_frame(removed_for_them),
        vec![WsOutcome::Event(WsEvent::UserRemovedFromChannel {
            channel_id: ChannelId::new("c1"),
            user_id: UserId::new("u-oscar"),
        })]
    );
}
