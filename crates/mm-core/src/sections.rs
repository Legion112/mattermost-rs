//! The sidebar projection: the user's channels, grouped the way the server
//! says they are grouped.
//!
//! Mattermost keeps the grouping and the ordering server-side, so it follows
//! the user between the web app, the phone and this client. A client that
//! invents its own flat list shows them a sidebar that matches nothing else
//! they use, which is the single most visible way to look wrong.

use crate::store::SidebarRow;
use mm_types::{CategoryKind, CategorySorting, ChannelCategory, ChannelId};
use std::collections::HashMap;
use std::hash::BuildHasher;

/// One group of channels under its heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarSection {
    pub display_name: String,
    pub kind: CategoryKind,
    /// The user muted the whole category.
    pub muted: bool,
    /// The user folded it shut. Carried through rather than acted on here:
    /// what a collapsed category still shows (its unread channels) is a
    /// display decision.
    pub collapsed: bool,
    pub rows: Vec<SidebarRow>,
}

impl SidebarSection {
    /// Whether anything in the section wants attention. This is what a
    /// collapsed category shows instead of its contents.
    #[must_use]
    pub fn is_unread(&self) -> bool {
        self.rows.iter().any(SidebarRow::is_unread)
    }

    #[must_use]
    pub fn mentions(&self) -> i64 {
        self.rows.iter().map(|r| r.unread.mentions).sum()
    }
}

/// Group `rows` under `categories`, which must already be in the server's
/// stacking order.
///
/// `recency` supplies each channel's last post time, which only the `Recent`
/// sorting needs; it is passed in rather than read off the row so that a
/// `SidebarRow` stays the thing the sidebar draws and nothing more.
#[must_use]
pub fn group<S: BuildHasher>(
    rows: Vec<SidebarRow>,
    categories: &[ChannelCategory],
    recency: &HashMap<ChannelId, i64, S>,
) -> Vec<SidebarSection> {
    let mut remaining: HashMap<ChannelId, SidebarRow> =
        rows.into_iter().map(|r| (r.channel.clone(), r)).collect();

    let mut sections: Vec<SidebarSection> = categories
        .iter()
        .map(|category| {
            let rows = category
                .channel_ids
                .iter()
                .filter_map(|id| remaining.remove(id))
                .collect();
            SidebarSection {
                display_name: category.display_name.clone(),
                kind: category.kind.clone(),
                muted: category.muted,
                collapsed: category.collapsed,
                rows,
            }
        })
        .collect();

    // A channel joined since the categories were fetched is in none of them.
    // The server files new channels the same way: DMs under Direct Messages,
    // everything else under Channels. Dropping them instead would make a
    // freshly joined channel invisible until the next sync.
    let mut orphans: Vec<SidebarRow> = remaining.into_values().collect();
    orphans.sort_by(|a, b| a.channel.cmp(&b.channel));
    for row in orphans {
        // The kind and the heading are chosen together: a heading invented
        // for a kind that cannot be orphaned is a branch no test can reach,
        // and the coverage gate says so.
        let (wanted, heading) = if row.kind.is_dm() {
            (CategoryKind::DirectMessages, "Direct Messages")
        } else {
            (CategoryKind::Channels, "Channels")
        };
        match sections.iter_mut().find(|s| s.kind == wanted) {
            Some(section) => section.rows.push(row),
            None => sections.push(SidebarSection {
                display_name: heading.to_owned(),
                kind: wanted,
                muted: false,
                collapsed: false,
                rows: vec![row],
            }),
        }
    }

    for (section, category) in sections.iter_mut().zip(categories) {
        sort_rows(
            &mut section.rows,
            &category.sorting,
            &category.channel_ids,
            recency,
        );
    }
    // A section invented for orphans has no category to sort by; alphabetical
    // is the server's default and the only order it could be in.
    for section in sections.iter_mut().skip(categories.len()) {
        sort_rows(&mut section.rows, &CategorySorting::Alpha, &[], recency);
    }

    // An empty Favorites or custom category is hidden rather than drawn as a
    // heading with nothing under it, which is what the web app does.
    sections.retain(|s| !s.rows.is_empty());
    sections
}

/// Order one category's channels.
///
/// `Manual` keeps the order the user dragged them into, which is the order
/// `channel_ids` is already in, so it only has to place the orphans appended
/// after it. Everything else is a sort.
fn sort_rows<S: BuildHasher>(
    rows: &mut [SidebarRow],
    sorting: &CategorySorting,
    manual: &[ChannelId],
    recency: &HashMap<ChannelId, i64, S>,
) {
    match sorting {
        CategorySorting::Manual => {
            let rank = |id: &ChannelId| manual.iter().position(|c| c == id).unwrap_or(usize::MAX);
            rows.sort_by(|a, b| {
                rank(&a.channel)
                    .cmp(&rank(&b.channel))
                    .then_with(|| by_name(a, b))
            });
        }
        CategorySorting::Recent => rows.sort_by(|a, b| {
            let at = |r: &SidebarRow| recency.get(&r.channel).copied().unwrap_or(0);
            at(b).cmp(&at(a)).then_with(|| by_name(a, b))
        }),
        // `Default` is the empty string the server sends for a category the
        // user has never reordered, and the web app shows those
        // alphabetically. An unknown sorting lands here too: a strange order
        // is survivable, a missing channel is not.
        CategorySorting::Default | CategorySorting::Alpha | CategorySorting::Other(_) => {
            rows.sort_by(by_name);
        }
    }
}

/// Case-insensitive, then by id so the order is total and the goldens are
/// stable. A channel named "zebra" must not sort above "Apple".
fn by_name(a: &SidebarRow, b: &SidebarRow) -> std::cmp::Ordering {
    a.display_name
        .to_lowercase()
        .cmp(&b.display_name.to_lowercase())
        .then_with(|| a.channel.cmp(&b.channel))
}

#[cfg(test)]
#[path = "tests/sections.rs"]
mod tests;
