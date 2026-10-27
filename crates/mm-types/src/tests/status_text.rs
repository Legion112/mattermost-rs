use super::*;

/// Shapes captured from a live 12.0.1 server: a status with words, a
/// status that is only an emoji, and one that has been cleared.
#[test]
fn the_live_shapes_parse() {
    let holiday = CustomStatus::parse(
        r#"{"emoji":"palm_tree","text":"On a vacation","duration":"this_week","expires_at":"2026-04-11T20:59:59.999Z"}"#,
    )
    .expect("a status");
    assert_eq!(holiday.emoji, "palm_tree");
    assert_eq!(holiday.text, "On a vacation");

    // An emoji and nothing else is a status; Mattermost draws it.
    let wordless = CustomStatus::parse(
        r#"{"emoji":"sunglasses","text":"","duration":"today","expires_at":"2024-09-28T05:00:48.574Z"}"#,
    )
    .expect("a status");
    assert_eq!(wordless.emoji, "sunglasses");

    // Cleared, and nonsense, say nothing.
    assert!(CustomStatus::parse(r#"{"emoji":"","text":"  "}"#).is_none());
    assert!(CustomStatus::parse("not json").is_none());
    assert!(CustomStatus::parse("").is_none());
    // Every derive is exercised.
    assert_eq!(holiday, holiday.clone());
    assert!(!format!("{holiday:?}").is_empty());
    assert_eq!(CustomStatus::default(), CustomStatus::default());
}

/// Most of the statuses on a real server have run out: people set
/// "Out for lunch" for an hour and never clear it. A client that
/// ignores `expires_at` shows last October's lunch.
#[test]
fn an_expired_status_says_so() {
    let lunch = CustomStatus::parse(
        r#"{"emoji":"hamburger","text":"Out for lunch","expires_at":"2026-10-20T14:23:00Z"}"#,
    )
    .expect("a status");
    assert!(lunch.is_expired(UtcMoment::parse("2026-10-27T00:00:00Z").unwrap()));
    assert!(!lunch.is_expired(UtcMoment::parse("2026-10-20T14:22:59Z").unwrap()));
    // The moment itself counts as over: a status good "until 14:23"
    // is not still good at 14:23.
    assert!(lunch.is_expired(UtcMoment::parse("2026-10-20T14:23:00Z").unwrap()));

    // No expiry is written as the year-one sentinel rather than an
    // absent field, and never runs out.
    let forever = CustomStatus::parse(
        r#"{"emoji":"rocket","text":"","duration":"","expires_at":"0001-01-01T00:00:00Z"}"#,
    )
    .expect("a status");
    assert!(!forever.is_expired(UtcMoment::parse("2026-10-27T00:00:00Z").unwrap()));

    // And so is an expiry nobody can read: hiding what somebody wrote
    // is worse than showing it a little too long.
    let odd = CustomStatus {
        expires_at: "whenever".to_owned(),
        ..forever
    };
    assert!(!odd.is_expired(UtcMoment::parse("2026-10-27T00:00:00Z").unwrap()));
}

/// Only the shape the server writes, and nothing that would panic on
/// a short or malformed string.
#[test]
fn reading_an_instant_is_total() {
    assert!(UtcMoment::parse("2026-10-27T12:40:00Z").is_some());
    assert!(UtcMoment::parse("2026-10-27T12:40:00.574Z").is_some());
    assert!(UtcMoment::parse("2026-10-27T12:40:00").is_some());
    for bad in [
        "",
        "2026",
        "2026-10-27",
        "2026-10-27 12:40:00",
        "xxxx-xx-xxTxx:xx:xx",
    ] {
        assert!(UtcMoment::parse(bad).is_none(), "for {bad}");
    }
    // Ordering is what it is for.
    let early = UtcMoment::parse("2026-01-01T00:00:00Z").unwrap();
    let late = UtcMoment::parse("2026-12-31T23:59:59Z").unwrap();
    assert!(early < late);
    assert_eq!(early, early);
    assert!(!format!("{early:?}").is_empty());
}

/// Reached through the user, which is where a client actually asks:
/// the prop is a JSON document inside a JSON string, and an expired
/// one is no status at all.
#[test]
fn a_users_custom_status_comes_off_their_props() {
    let now = UtcMoment::parse("2026-10-27T00:00:00Z").unwrap();
    let mut user = crate::User::default();
    assert!(user.custom_status(now).is_none());

    user.props.insert(
        "customStatus".to_owned(),
        r#"{"emoji":"palm_tree","text":"On a vacation","expires_at":"2026-12-01T00:00:00Z"}"#
            .to_owned(),
    );
    assert_eq!(
        user.custom_status(now).map(|s| s.text),
        Some("On a vacation".to_owned())
    );

    user.props.insert(
        "customStatus".to_owned(),
        r#"{"emoji":"hamburger","text":"Out for lunch","expires_at":"2026-10-20T14:23:00Z"}"#
            .to_owned(),
    );
    assert!(user.custom_status(now).is_none());
}

/// `props: null` is the shape the server sends for somebody who has
/// none, and `#[serde(default)]` does not cover a key that is
/// present and null -- the trap this whole tree has been bitten by.
#[test]
fn null_props_parse_as_none() {
    let user: crate::User =
        serde_json::from_str(r#"{"id":"u1","username":"a","props":null}"#).expect("parses");
    assert!(user.props.is_empty());
}
