use super::*;

#[test]
fn a_bare_server_link_parses() {
    assert_eq!(
        parse("mattermost://chat.example.com").unwrap(),
        DeepLink::Server {
            host: "chat.example.com".into()
        }
    );
}

#[test]
fn a_channel_link_parses() {
    let link = parse("mattermost://chat.example.com/alpha/channels/town-square").unwrap();
    assert_eq!(
        link,
        DeepLink::Channel {
            host: "chat.example.com".into(),
            team: "alpha".into(),
            channel: "town-square".into(),
        }
    );
    assert_eq!(link.host(), "chat.example.com");
}

#[test]
fn a_permalink_parses() {
    let link = parse("mattermost://chat.example.com/alpha/pl/abcdefghijklmnopqrstuvwxyz").unwrap();
    assert_eq!(
        link,
        DeepLink::Permalink {
            host: "chat.example.com".into(),
            team: "alpha".into(),
            post: PostId::new("abcdefghijklmnopqrstuvwxyz"),
        }
    );
    assert_eq!(link.host(), "chat.example.com");
}

/// Desktop environments pass these as an argv string, so stray whitespace and
/// trailing slashes are routine rather than exceptional.
#[test]
fn surrounding_whitespace_and_trailing_slashes_are_tolerated() {
    assert_eq!(
        parse("  mattermost://host/alpha/channels/town-square/  ").unwrap(),
        DeepLink::Channel {
            host: "host".into(),
            team: "alpha".into(),
            channel: "town-square".into(),
        }
    );
    assert_eq!(
        parse("mattermost://host/").unwrap(),
        DeepLink::Server {
            host: "host".into()
        }
    );
}

/// A query or fragment carries no routing information and must not defeat the
/// match.
#[test]
fn a_query_or_fragment_is_ignored() {
    for raw in [
        "mattermost://host/alpha/channels/town-square?from=email",
        "mattermost://host/alpha/channels/town-square#top",
    ] {
        assert_eq!(
            parse(raw).unwrap(),
            DeepLink::Channel {
                host: "host".into(),
                team: "alpha".into(),
                channel: "town-square".into(),
            },
            "{raw}"
        );
    }
}

/// An http link is the most likely wrong input — a user pasting a browser URL
/// — so the error has to name what was expected.
#[test]
fn a_non_mattermost_scheme_is_rejected_by_name() {
    let err = parse("https://chat.example.com/alpha/channels/town-square").unwrap_err();
    assert_eq!(
        err,
        DeepLinkError::WrongScheme("https://chat.example.com/alpha/channels/town-square".into())
    );
    assert!(err.to_string().contains("mattermost://"));
}

#[test]
fn a_link_with_no_host_is_rejected() {
    let err = parse("mattermost://").unwrap_err();
    assert_eq!(err, DeepLinkError::NoHost);
    assert!(err.to_string().contains("names no server"));
}

/// An unknown path shape must be reported rather than guessed at: opening the
/// wrong channel is worse than saying the link was not understood.
#[test]
fn an_unknown_path_shape_is_reported() {
    for raw in [
        "mattermost://host/alpha/boards/xyz",
        "mattermost://host/alpha",
        "mattermost://host/a/b/c/d",
    ] {
        match parse(raw) {
            Err(DeepLinkError::Unrecognised(path)) => assert!(!path.is_empty(), "{raw}"),
            other => panic!("{raw} gave {other:?}"),
        }
    }
    assert!(
        parse("mattermost://host/a/b/c/d")
            .unwrap_err()
            .to_string()
            .contains("unrecognised")
    );
}

#[test]
fn every_derive_is_exercised() {
    let link = DeepLink::Server { host: "h".into() };
    assert_eq!(link, link.clone());
    assert!(!format!("{link:?}").is_empty());

    let err = DeepLinkError::NoHost;
    assert_eq!(err, err.clone());
    assert!(!format!("{err:?}").is_empty());
    let as_err: &dyn std::error::Error = &err;
    assert!(as_err.source().is_none());

    assert_ne!(
        DeepLink::Server { host: "a".into() },
        DeepLink::Server { host: "b".into() }
    );
}
