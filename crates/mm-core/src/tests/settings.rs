use super::*;

/// The bug this exists for: the server ships `username`, this client showed
/// nicknames, and every name in the window differed from what the rest of
/// the organisation sees.
#[test]
fn the_servers_default_decides_when_the_user_has_no_preference() {
    assert_eq!(
        name_format(Some("username"), false, None),
        NameFormat::Username
    );
    assert_eq!(
        name_format(Some("full_name"), false, None),
        NameFormat::FullName
    );
    assert_eq!(
        name_format(Some("nickname_full_name"), false, None),
        NameFormat::NicknameFullName
    );
}

/// A user who has chosen overrides the server's default.
#[test]
fn a_preference_overrides_the_default() {
    assert_eq!(
        name_format(Some("username"), false, Some("full_name")),
        NameFormat::FullName
    );
}

/// Unless the administrator locked it, in which case their choice stands for
/// everyone and a stale preference must not resurface.
#[test]
fn a_locked_setting_ignores_the_preference() {
    assert_eq!(
        name_format(Some("username"), true, Some("full_name")),
        NameFormat::Username
    );
}

/// An empty preference row is not a choice. The server stores preferences as
/// strings and an emptied one is a real state.
#[test]
fn an_empty_preference_is_not_a_choice() {
    assert_eq!(
        name_format(Some("full_name"), false, Some("")),
        NameFormat::FullName
    );
}

/// Nothing known at all still has to produce something, and the something is
/// the username -- the only field every account is guaranteed to have.
#[test]
fn knowing_nothing_falls_back_to_the_username() {
    assert_eq!(name_format(None, false, None), NameFormat::Username);
    assert_eq!(
        name_format(Some("something_new"), false, None),
        NameFormat::Username
    );
}

/// `always_on` and `disabled` decide outright: the preference cannot reach
/// past an administrator who has settled the question.
#[test]
fn the_two_absolute_thread_settings_ignore_the_preference() {
    assert_eq!(
        threading(Some("always_on"), Some("off")),
        Threading::Collapsed
    );
    assert_eq!(threading(Some("disabled"), Some("on")), Threading::Flat);
}

/// `default_on` and `default_off` only say which way an *unexpressed*
/// preference falls. Reading `default_on` as "on" regardless -- which is
/// what this client did -- hides every reply from someone who turned threads
/// off on purpose.
#[test]
fn the_two_default_thread_settings_let_the_user_decide() {
    assert_eq!(threading(Some("default_on"), None), Threading::Collapsed);
    assert_eq!(threading(Some("default_on"), Some("off")), Threading::Flat);
    assert_eq!(threading(Some("default_off"), None), Threading::Flat);
    assert_eq!(
        threading(Some("default_off"), Some("on")),
        Threading::Collapsed
    );
}

/// The preference is stored as a string and both spellings are seen in the
/// wild.
#[test]
fn both_spellings_of_the_thread_preference_are_understood() {
    assert_eq!(
        threading(Some("default_off"), Some("true")),
        Threading::Collapsed
    );
    assert_eq!(
        threading(Some("default_on"), Some("false")),
        Threading::Flat
    );
    // Anything else is not an answer, so the default stands.
    assert_eq!(
        threading(Some("default_on"), Some("maybe")),
        Threading::Collapsed
    );
}

/// A server that said nothing is an older one, and older servers had no
/// collapsed threads at all.
#[test]
fn an_unknown_server_setting_leaves_threads_flat() {
    assert_eq!(threading(None, None), Threading::Flat);
}
