use super::*;
use mm_types::{PostId, Timestamp};

const ME: &str = "u-me";

fn post(author: &str, message: &str) -> Post {
    Post {
        id: PostId::new("p1"),
        user_id: UserId::new(author),
        channel_id: ChannelId::new("c1"),
        create_at: Timestamp(10),
        message: message.to_owned(),
        ..Post::default()
    }
}

fn decide(
    post: &Post,
    mentioned: bool,
    focused: Option<&ChannelId>,
    settings: &NotifySettings,
) -> Result<Notification, Suppressed> {
    notification_for(
        post,
        &UserId::new(ME),
        "Bob",
        "Town Square",
        mentioned,
        focused,
        settings,
    )
}

#[test]
fn an_ordinary_message_notifies() {
    let n = decide(
        &post("u-bob", "hello"),
        false,
        None,
        &NotifySettings::default(),
    )
    .unwrap();
    assert_eq!(n.title, "Bob in Town Square");
    assert_eq!(n.body, "hello");
    assert_eq!(n.channel, ChannelId::new("c1"));
    assert!(!n.is_mention);
}

/// Self-notification on every message you send is one of the most irritating
/// bugs a chat client can have.
#[test]
fn your_own_post_never_notifies() {
    assert_eq!(
        decide(&post(ME, "hi"), false, None, &NotifySettings::default()),
        Err(Suppressed::OwnPost)
    );
}

#[test]
fn join_and_leave_notices_are_suppressed_by_default() {
    let mut p = post("u-bob", "bob joined the channel");
    p.kind = "system_join_channel".into();
    assert_eq!(
        decide(&p, false, None, &NotifySettings::default()),
        Err(Suppressed::SystemMessage)
    );

    let noisy = NotifySettings {
        suppress_system: false,
        ..NotifySettings::default()
    };
    assert!(decide(&p, false, None, &noisy).is_ok());
}

/// A delete event can arrive before the notification is raised.
#[test]
fn a_deleted_post_never_notifies() {
    let mut p = post("u-bob", "gone");
    p.delete_at = Timestamp(20);
    assert_eq!(
        decide(&p, false, None, &NotifySettings::default()),
        Err(Suppressed::Deleted)
    );
}

/// The channel setting wins over the global one; `Inherit` is the wire default
/// and must fall through rather than being read as a value.
#[test]
fn the_channel_setting_overrides_the_global_one() {
    let quiet_channel = NotifySettings {
        global: DesktopNotify::All,
        channel: DesktopNotify::Silent,
        ..NotifySettings::default()
    };
    assert_eq!(quiet_channel.effective(), DesktopNotify::Silent);
    assert_eq!(
        decide(&post("u-bob", "hi"), false, None, &quiet_channel),
        Err(Suppressed::SettingIsNone)
    );

    let inheriting = NotifySettings {
        global: DesktopNotify::Mention,
        channel: DesktopNotify::Inherit,
        ..NotifySettings::default()
    };
    assert_eq!(inheriting.effective(), DesktopNotify::Mention);

    // A loud channel inside a quiet account still notifies.
    let loud_channel = NotifySettings {
        global: DesktopNotify::Silent,
        channel: DesktopNotify::All,
        ..NotifySettings::default()
    };
    assert!(decide(&post("u-bob", "hi"), false, None, &loud_channel).is_ok());
}

/// Neither level configured means "all" — the server's own default.
#[test]
fn inheriting_all_the_way_up_means_everything_notifies() {
    let unset = NotifySettings {
        global: DesktopNotify::Inherit,
        channel: DesktopNotify::Inherit,
        ..NotifySettings::default()
    };
    assert_eq!(unset.effective(), DesktopNotify::All);
    assert!(decide(&post("u-bob", "hi"), false, None, &unset).is_ok());
}

#[test]
fn mentions_only_lets_a_mention_through_and_stops_the_rest() {
    let settings = NotifySettings {
        global: DesktopNotify::Mention,
        ..NotifySettings::default()
    };
    assert_eq!(
        decide(&post("u-bob", "chatter"), false, None, &settings),
        Err(Suppressed::MentionsOnlyAndNotMentioned)
    );
    let n = decide(&post("u-bob", "@me look"), true, None, &settings).unwrap();
    assert!(n.is_mention);
}

/// Muting silences ordinary traffic but must not swallow a message that names
/// the reader — losing that is the thing people never forgive.
#[test]
fn a_muted_channel_is_silent_except_for_mentions() {
    let muted = NotifySettings {
        muted: true,
        ..NotifySettings::default()
    };
    assert_eq!(
        decide(&post("u-bob", "chatter"), false, None, &muted),
        Err(Suppressed::Muted)
    );
    assert!(decide(&post("u-bob", "@me hi"), true, None, &muted).is_ok());
}

/// Whether mute alone silences notifications is not confirmed against a live
/// server, so it is a flag rather than a hidden assumption.
#[test]
fn mute_can_be_configured_not_to_silence() {
    let muted_but_loud = NotifySettings {
        muted: true,
        muted_is_silent: false,
        ..NotifySettings::default()
    };
    assert!(decide(&post("u-bob", "chatter"), false, None, &muted_but_loud).is_ok());
}

/// Notifying about a message already on screen is noise.
#[test]
fn the_channel_being_looked_at_does_not_notify() {
    let here = ChannelId::new("c1");
    assert_eq!(
        decide(
            &post("u-bob", "hi"),
            false,
            Some(&here),
            &NotifySettings::default()
        ),
        Err(Suppressed::AlreadyLooking)
    );

    // Another channel being focused is irrelevant.
    let elsewhere = ChannelId::new("c2");
    assert!(
        decide(
            &post("u-bob", "hi"),
            false,
            Some(&elsewhere),
            &NotifySettings::default()
        )
        .is_ok()
    );

    let always = NotifySettings {
        notify_when_focused: true,
        ..NotifySettings::default()
    };
    assert!(decide(&post("u-bob", "hi"), false, Some(&here), &always).is_ok());
}

#[test]
fn a_dm_with_no_channel_name_is_titled_with_the_author_alone() {
    let n = notification_for(
        &post("u-bob", "hi"),
        &UserId::new(ME),
        "Bob",
        "",
        false,
        None,
        &NotifySettings::default(),
    )
    .unwrap();
    assert_eq!(n.title, "Bob");
}

#[test]
fn desktop_notify_parses_its_wire_values_and_inherits_anything_unknown() {
    assert_eq!(DesktopNotify::from_prop("all"), DesktopNotify::All);
    assert_eq!(DesktopNotify::from_prop("mention"), DesktopNotify::Mention);
    assert_eq!(DesktopNotify::from_prop("none"), DesktopNotify::Silent);
    for raw in ["default", "", "something_new"] {
        assert_eq!(
            DesktopNotify::from_prop(raw),
            DesktopNotify::Inherit,
            "{raw}"
        );
    }
    assert_eq!(DesktopNotify::default(), DesktopNotify::Inherit);
}

#[test]
fn short_messages_are_passed_through_with_newlines_flattened() {
    assert_eq!(summarize("hello"), "hello");
    assert_eq!(summarize("  padded  "), "padded");
    assert_eq!(summarize("two\nlines"), "two lines");
    assert_eq!(summarize(""), "");
}

/// Truncating mid-UTF-8 panics, and truncating mid-word looks broken, so the
/// cut is on a character boundary at a space.
#[test]
fn long_messages_are_cut_on_a_word_and_a_character_boundary() {
    let long = "словослово ".repeat(40);
    let short = summarize(&long);
    assert!(short.chars().count() <= 181, "{}", short.chars().count());
    assert!(short.ends_with('…'));

    // No space to break on: it still must not panic or split a character.
    let unbroken = "é".repeat(400);
    let cut = summarize(&unbroken);
    assert!(cut.ends_with('…'));
    assert!(cut.chars().count() <= 181);
}

#[test]
fn every_derive_is_exercised() {
    let settings = NotifySettings::default();
    assert!(!format!("{settings:?}").is_empty());
    assert!(!format!("{:?}", settings.clone()).is_empty());

    let n = decide(&post("u-bob", "hi"), false, None, &settings).unwrap();
    assert_eq!(n, n.clone());
    assert!(!format!("{n:?}").is_empty());

    let s = Suppressed::OwnPost;
    assert_eq!(s, s.clone());
    assert!(!format!("{s:?}").is_empty());
    assert_ne!(Suppressed::Muted, Suppressed::Deleted);

    let d = DesktopNotify::All;
    assert_eq!(d, d.clone());
    assert!(!format!("{d:?}").is_empty());
}

/// Whether to make a sound is a separate setting from whether to
/// notify, and it travels on the notification: somebody who turned
/// the sound off still wants the banner, and somebody with banners
/// switched off at the desktop still hears the sound -- which for
/// most people is the whole notification.
#[test]
fn the_sound_setting_rides_along_without_deciding_anything() {
    let post = Post {
        id: PostId::new("p1"),
        channel_id: ChannelId::new("c1"),
        user_id: UserId::new("them"),
        message: "hello".to_owned(),
        ..Post::default()
    };
    let me = UserId::new("me");
    let settings = |sound: bool| NotifySettings {
        sound,
        ..NotifySettings::default()
    };

    let loud = notification_for(&post, &me, "them", "c1", false, None, &settings(true))
        .expect("notifies");
    assert!(loud.sound);
    let quiet = notification_for(&post, &me, "them", "c1", false, None, &settings(false))
        .expect("still notifies");
    assert!(!quiet.sound);
    // Only the sound differs: turning it off is not turning the
    // notification off.
    assert_eq!(loud.title, quiet.title);
    assert_eq!(loud.body, quiet.body);
}
