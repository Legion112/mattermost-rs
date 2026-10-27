use super::*;
use std::path::PathBuf;

/// The three names that have to agree -- the GTK application id, the
/// icon, and `StartupWMClass` -- and the `%u` without which a
/// `mattermost://` link is accepted and then dropped.
///
/// A wrong `StartupWMClass` fails quietly: the window never matches its
/// launcher, so it gets a second icon in the dock and no unread badge,
/// and nothing anywhere says why.
#[test]
fn the_entry_names_the_application_the_same_way_three_times() {
    let entry = render(&PathBuf::from("/usr/bin/mattermost-rs"));
    assert!(entry.starts_with("[Desktop Entry]\n"));
    assert!(
        entry.contains("Exec=/usr/bin/mattermost-rs %u\n"),
        "{entry}"
    );
    assert!(entry.contains(&format!("Icon={APP_ID}\n")), "{entry}");
    assert!(
        entry.contains(&format!("StartupWMClass={APP_ID}\n")),
        "{entry}"
    );
    assert!(
        entry.contains("MimeType=x-scheme-handler/mattermost;\n"),
        "{entry}"
    );
    // Every line is a key=value or the header, which is what keeps the
    // file parseable by anything reading it.
    for line in entry.lines().skip(1) {
        assert!(line.contains('='), "{line:?}");
    }
    assert!(entry.ends_with('\n'));
}

/// A path with a space in it is a command plus an argument unless it is
/// quoted, which is how an application installed from a build directory
/// fails to start with no message at all.
#[test]
fn a_path_needing_quotes_gets_them() {
    let spaced = render(&PathBuf::from("/home/me/My Builds/mattermost-rs"));
    assert!(
        spaced.contains("Exec=\"/home/me/My Builds/mattermost-rs\" %u\n"),
        "{spaced}"
    );

    // Inside the quotes, the reserved characters are escaped too.
    let awkward = render(&PathBuf::from(r#"/home/me/a"b$c/mattermost-rs"#));
    assert!(
        awkward.contains(r#"Exec="/home/me/a\"b\$c/mattermost-rs" %u"#),
        "{awkward}"
    );

    // An ordinary path is left exactly as it is.
    let plain = render(&PathBuf::from("/opt/mattermost-rs/bin/mattermost-rs"));
    assert!(!plain.contains('"'), "{plain}");
}

/// The badge counts mentions, not unread messages -- things addressed
/// to this person, not channels that are merely busy -- and a badge
/// showing zero is worse than none, because it reads as something to
/// attend to.
#[test]
fn the_badge_shows_mentions_and_hides_itself_at_zero() {
    assert_eq!(
        Badge::for_mentions(3),
        Badge {
            count: 3,
            visible: true
        }
    );
    assert!(!Badge::for_mentions(0).visible);
    assert_eq!(Badge::for_mentions(0).count, 0);
    // A negative count can only come from arithmetic that has already
    // gone wrong; the dock must not be asked to draw "-1".
    assert_eq!(Badge::for_mentions(-5).count, 0);
    assert!(!Badge::for_mentions(-5).visible);

    assert_eq!(Badge::default(), Badge::for_mentions(0));
    let one = Badge::for_mentions(1);
    assert_eq!(one, one);
    assert!(!format!("{one:?}").is_empty());
}
