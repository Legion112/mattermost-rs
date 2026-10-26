use super::*;
use crate::unread::Unread;
use mm_types::ChannelType;

fn row(id: &str, name: &str) -> SidebarRow {
    SidebarRow {
        channel: ChannelId::new(id),
        display_name: name.to_owned(),
        kind: ChannelType::Open,
        unread: Unread::default(),
        muted: false,
    }
}

fn dm(id: &str, name: &str) -> SidebarRow {
    SidebarRow {
        kind: ChannelType::Direct,
        ..row(id, name)
    }
}

fn category(id: &str, name: &str, kind: CategoryKind, channels: &[&str]) -> ChannelCategory {
    ChannelCategory {
        id: id.to_owned(),
        display_name: name.to_owned(),
        kind,
        channel_ids: channels.iter().map(|c| ChannelId::new(*c)).collect(),
        ..ChannelCategory::default()
    }
}

fn names(sections: &[SidebarSection]) -> Vec<(String, Vec<String>)> {
    sections
        .iter()
        .map(|s| {
            (
                s.display_name.clone(),
                s.rows.iter().map(|r| r.display_name.clone()).collect(),
            )
        })
        .collect()
}

fn no_recency() -> HashMap<ChannelId, i64> {
    HashMap::new()
}

/// The ordinary case: the server's groups, in the server's order.
#[test]
fn channels_land_in_the_category_the_server_put_them_in() {
    let cats = [
        category("fav", "Favorites", CategoryKind::Favorites, &["c3"]),
        category("ch", "Channels", CategoryKind::Channels, &["c1", "c2"]),
    ];
    let rows = vec![row("c1", "Alpha"), row("c2", "Beta"), row("c3", "Gamma")];
    assert_eq!(
        names(&group(rows, &cats, &no_recency())),
        [
            ("Favorites".to_owned(), vec!["Gamma".to_owned()]),
            (
                "Channels".to_owned(),
                vec!["Alpha".to_owned(), "Beta".to_owned()]
            ),
        ]
    );
}

/// A category with nothing in it is not drawn. The web app hides an empty
/// Favorites rather than showing a heading with a blank under it.
#[test]
fn an_empty_category_is_not_drawn() {
    let cats = [
        category("fav", "Favorites", CategoryKind::Favorites, &[]),
        category("ch", "Channels", CategoryKind::Channels, &["c1"]),
    ];
    let sections = group(vec![row("c1", "Alpha")], &cats, &no_recency());
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].display_name, "Channels");
}

/// A channel joined since the categories were fetched is in none of them.
/// It has to appear somewhere or it is invisible until the next sync -- and
/// a DM belongs under Direct Messages, not under Channels.
#[test]
fn a_channel_in_no_category_is_filed_the_way_the_server_would_file_it() {
    let cats = [
        category("ch", "Channels", CategoryKind::Channels, &["c1"]),
        category("dm", "Direct Messages", CategoryKind::DirectMessages, &[]),
    ];
    let rows = vec![row("c1", "Alpha"), row("c9", "Brand New"), dm("d9", "bob")];
    assert_eq!(
        names(&group(rows, &cats, &no_recency())),
        [
            (
                "Channels".to_owned(),
                vec!["Alpha".to_owned(), "Brand New".to_owned()]
            ),
            ("Direct Messages".to_owned(), vec!["bob".to_owned()]),
        ]
    );
}

/// With no categories at all -- before the first sync returns -- every
/// channel still has to be reachable, under a heading invented for it.
#[test]
fn with_no_categories_everything_still_appears() {
    let rows = vec![row("c1", "Alpha"), dm("d1", "bob")];
    assert_eq!(
        names(&group(rows, &[], &no_recency())),
        [
            ("Channels".to_owned(), vec!["Alpha".to_owned()]),
            ("Direct Messages".to_owned(), vec!["bob".to_owned()]),
        ]
    );
}

/// The server's default sorting is the empty string, and the web app shows
/// those alphabetically and case-insensitively. "zebra" must not sort above
/// "Apple" just because of its capital letter.
#[test]
fn the_default_sorting_is_alphabetical_and_ignores_case() {
    let cats = [category(
        "ch",
        "Channels",
        CategoryKind::Channels,
        &["c1", "c2", "c3"],
    )];
    let rows = vec![row("c1", "zebra"), row("c2", "Apple"), row("c3", "mango")];
    assert_eq!(
        names(&group(rows, &cats, &no_recency()))[0].1,
        ["Apple", "mango", "zebra"]
    );
}

/// A sorting we do not model must not reorder anything into nothing: it
/// falls back to alphabetical rather than dropping channels.
#[test]
fn an_unknown_sorting_falls_back_to_alphabetical() {
    let mut cat = category("ch", "Channels", CategoryKind::Channels, &["c1", "c2"]);
    cat.sorting = CategorySorting::Other("by_vibes".into());
    let rows = vec![row("c1", "zebra"), row("c2", "Apple")];
    assert_eq!(
        names(&group(rows, &[cat], &no_recency()))[0].1,
        ["Apple", "zebra"]
    );
}

/// Manual sorting is the whole point of letting a user drag channels about,
/// so `channel_ids` order wins over the alphabet.
#[test]
fn manual_sorting_keeps_the_order_the_user_dragged_them_into() {
    let mut cat = category(
        "ch",
        "Channels",
        CategoryKind::Channels,
        &["c3", "c1", "c2"],
    );
    cat.sorting = CategorySorting::Manual;
    let rows = vec![row("c1", "Alpha"), row("c2", "Beta"), row("c3", "Gamma")];
    assert_eq!(
        names(&group(rows, &[cat], &no_recency()))[0].1,
        ["Gamma", "Alpha", "Beta"]
    );
}

/// A channel the user has not placed yet sorts after the ones they have,
/// rather than landing arbitrarily in the middle of their arrangement.
#[test]
fn manual_sorting_puts_an_unplaced_channel_last() {
    let mut cat = category("ch", "Channels", CategoryKind::Channels, &["c2"]);
    cat.sorting = CategorySorting::Manual;
    let rows = vec![row("c1", "Alpha"), row("c2", "Beta")];
    assert_eq!(
        names(&group(rows, &[cat], &no_recency()))[0].1,
        ["Beta", "Alpha"]
    );
}

/// Direct Messages sort by most recent, which is the server's default for
/// that category and the only order that makes a DM list usable.
#[test]
fn recent_sorting_puts_the_newest_conversation_first() {
    let mut cat = category(
        "dm",
        "Direct Messages",
        CategoryKind::DirectMessages,
        &["d1", "d2", "d3"],
    );
    cat.sorting = CategorySorting::Recent;
    let recency = HashMap::from([
        (ChannelId::new("d1"), 100),
        (ChannelId::new("d2"), 300),
        // d3 has no recorded last post and must still appear, oldest.
    ]);
    let rows = vec![dm("d1", "alice"), dm("d2", "bob"), dm("d3", "carol")];
    assert_eq!(
        names(&group(rows, &[cat], &recency))[0].1,
        ["bob", "alice", "carol"]
    );
}

/// A collapsed category still has to say whether anything inside it wants
/// attention, because that is all the user can see of it.
#[test]
fn a_section_reports_the_unread_state_of_its_contents() {
    let mut loud = row("c1", "Alpha");
    loud.unread = Unread {
        messages: 3,
        mentions: 2,
    };
    let quiet = row("c2", "Beta");
    let cat = category("ch", "Channels", CategoryKind::Channels, &["c1", "c2"]);
    let sections = group(vec![loud, quiet], &[cat], &no_recency());
    assert!(sections[0].is_unread());
    assert_eq!(sections[0].mentions(), 2);

    let silent = group(
        vec![row("c3", "Gamma")],
        &[category("ch", "Channels", CategoryKind::Channels, &["c3"])],
        &no_recency(),
    );
    assert!(!silent[0].is_unread());
    assert_eq!(silent[0].mentions(), 0);
}

/// A muted or collapsed category carries those flags through to the view,
/// which is the only place they mean anything.
#[test]
fn muted_and_collapsed_travel_with_the_section() {
    let mut cat = category("ch", "Channels", CategoryKind::Channels, &["c1"]);
    cat.muted = true;
    cat.collapsed = true;
    let sections = group(vec![row("c1", "Alpha")], &[cat], &no_recency());
    assert!(sections[0].muted);
    assert!(sections[0].collapsed);
    assert_eq!(sections[0].kind, CategoryKind::Channels);
}

/// A Favorites category with no Channels category beside it still has to
/// take the leftovers, under a heading of its own.
#[test]
fn an_orphan_invents_only_the_heading_it_needs() {
    let cats = [category(
        "fav",
        "Favorites",
        CategoryKind::Favorites,
        &["c1"],
    )];
    let rows = vec![row("c1", "Alpha"), row("c2", "Beta")];
    assert_eq!(
        names(&group(rows, &cats, &no_recency())),
        [
            ("Favorites".to_owned(), vec!["Alpha".to_owned()]),
            ("Channels".to_owned(), vec!["Beta".to_owned()]),
        ]
    );
}
