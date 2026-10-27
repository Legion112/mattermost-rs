use super::*;

/// The tooltip is the one line a tray icon gets, so it carries the
/// count rather than repeating the application's name at you. Urgency
/// is reserved for mentions: an icon that is always shouting is an
/// icon nobody reads.
#[test]
fn the_tooltip_says_how_many_are_waiting() {
    let quiet = Tray::for_mentions(0);
    assert_eq!(quiet.tooltip, "mattermost-rs");
    assert!(!quiet.urgent);

    assert_eq!(Tray::for_mentions(1).tooltip, "mattermost-rs — 1 mention");
    assert_eq!(Tray::for_mentions(4).tooltip, "mattermost-rs — 4 mentions");
    assert!(Tray::for_mentions(1).urgent);

    // A negative count can only come from arithmetic that has already
    // gone wrong, and must not reach the tooltip.
    assert_eq!(Tray::for_mentions(-2), quiet);
}

/// Two entries and no more. A tray menu is reached by accident as
/// often as on purpose, and everything on it is one slip from being
/// clicked -- which is why "Quit" is the only destructive thing there
/// and there is nothing next to it to mis-hit.
#[test]
fn the_menu_offers_exactly_opening_and_quitting() {
    let menu = Tray::menu();
    assert_eq!(
        menu,
        [("Open", TrayAction::Open), ("Quit", TrayAction::Quit)]
    );
    assert!(menu.iter().all(|(label, _)| !label.is_empty()));

    // Every derive is exercised.
    let one = Tray::for_mentions(1);
    assert_eq!(one, one.clone());
    assert!(!format!("{one:?}").is_empty());
    assert_eq!(TrayAction::Open, TrayAction::Open);
    assert_ne!(TrayAction::Open, TrayAction::Quit);
    assert!(!format!("{:?}", TrayAction::Quit).is_empty());
}
