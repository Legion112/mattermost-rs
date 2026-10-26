//! Sidebar categories — the server-side grouping of a user's channel list.
//!
//! Every Mattermost user has at least Favorites, Channels and Direct Messages,
//! and may have custom categories of their own. The grouping lives on the
//! server and follows the user between clients, so a client that invents its
//! own order shows them a sidebar that matches nothing else they use.

use crate::{ChannelId, TeamId, UserId};
use serde::{Deserialize, Serialize};

/// Which of the built-in groupings this is, or a user-made one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    Favorites,
    #[default]
    Channels,
    DirectMessages,
    Custom,
    /// Anything the server sent that we do not model.
    #[serde(untagged)]
    Other(String),
}

/// How the channels inside a category are ordered.
///
/// The server sends an **empty string** for a category the user has never
/// reordered, which is the common case and is not documented as a value.
/// Observed on 12.0.1 for both Favorites and Channels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySorting {
    /// `""` — the server's default. Treated as alphabetical, which is what
    /// the webapp shows for an untouched Channels category.
    #[serde(rename = "")]
    #[default]
    Default,
    Alpha,
    Recent,
    /// The user dragged the channels into an order; `channel_ids` is it.
    Manual,
    /// Anything the server sent that we do not model.
    #[serde(untagged)]
    Other(String),
}

/// One sidebar category as the server stores it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelCategory {
    /// Not a 26-character id: the built-in categories are named
    /// `favorites_{user}_{team}`, so this is deliberately a plain `String`
    /// rather than one of the id newtypes.
    pub id: String,
    pub user_id: UserId,
    pub team_id: TeamId,
    pub display_name: String,
    #[serde(rename = "type")]
    pub kind: CategoryKind,
    pub sorting: CategorySorting,
    pub sort_order: i64,
    pub muted: bool,
    pub collapsed: bool,
    /// In the user's manual order. A channel the user is in may be absent
    /// here if they joined it since this was fetched.
    pub channel_ids: Vec<ChannelId>,
}

/// The response of `GET /users/{user}/teams/{team}/channels/categories`.
///
/// `order` is authoritative for how the categories are stacked; `categories`
/// is not guaranteed to be in that order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CategoryList {
    pub categories: Vec<ChannelCategory>,
    pub order: Vec<String>,
}

impl CategoryList {
    /// The categories in the order the server says to stack them, with any
    /// category missing from `order` kept at the end rather than dropped.
    #[must_use]
    pub fn in_order(&self) -> Vec<&ChannelCategory> {
        let mut out: Vec<&ChannelCategory> = self
            .order
            .iter()
            .filter_map(|id| self.categories.iter().find(|c| &c.id == id))
            .collect();
        out.extend(
            self.categories
                .iter()
                .filter(|c| !self.order.contains(&c.id)),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from Mattermost 12.0.1. The empty `sorting` is the
    /// part worth keeping: it is what an untouched category actually sends,
    /// and it appears in no documentation.
    const LIVE: &str = r#"{
      "categories": [
        {"id":"channels_u_t","user_id":"u","team_id":"t","sort_order":10,"sorting":"",
         "type":"channels","display_name":"Channels","muted":false,"collapsed":false,
         "channel_ids":["c1","c2"]},
        {"id":"favorites_u_t","user_id":"u","team_id":"t","sort_order":0,"sorting":"",
         "type":"favorites","display_name":"Favorites","muted":false,"collapsed":false,
         "channel_ids":[]},
        {"id":"direct_messages_u_t","user_id":"u","team_id":"t","sort_order":20,
         "sorting":"recent","type":"direct_messages","display_name":"Direct Messages",
         "muted":false,"collapsed":false,"channel_ids":[]}
      ],
      "order": ["favorites_u_t","channels_u_t","direct_messages_u_t"]
    }"#;

    /// The live payload parses, including the undocumented empty `sorting`.
    #[test]
    fn the_live_payload_parses() {
        let list: CategoryList = serde_json::from_str(LIVE).expect("parses");
        assert_eq!(list.categories.len(), 3);
        let channels = &list.categories[0];
        assert_eq!(channels.kind, CategoryKind::Channels);
        assert_eq!(channels.sorting, CategorySorting::Default);
        assert_eq!(channels.channel_ids.len(), 2);
        assert_eq!(list.categories[2].sorting, CategorySorting::Recent);
        assert_eq!(list.categories[2].kind, CategoryKind::DirectMessages);
    }

    /// `categories` arrives in a different order from `order`, and `order` is
    /// the one the user sees. Reading the array as-is puts Channels above
    /// Favorites, which is what the live payload above would do.
    #[test]
    fn order_decides_the_stacking_not_the_array() {
        let list: CategoryList = serde_json::from_str(LIVE).expect("parses");
        let names: Vec<&str> = list
            .in_order()
            .iter()
            .map(|c| c.display_name.as_str())
            .collect();
        assert_eq!(names, ["Favorites", "Channels", "Direct Messages"]);
    }

    /// A category missing from `order` is kept at the end. Dropping it would
    /// hide every channel in it; the server has no reason to send one, but a
    /// sidebar that silently loses channels is not a failure worth risking.
    #[test]
    fn a_category_missing_from_order_is_kept_not_dropped() {
        let mut list: CategoryList = serde_json::from_str(LIVE).expect("parses");
        list.order.remove(1);
        let names: Vec<&str> = list
            .in_order()
            .iter()
            .map(|c| c.display_name.as_str())
            .collect();
        assert_eq!(names, ["Favorites", "Direct Messages", "Channels"]);
    }

    /// An unknown `type` or `sorting` keeps its raw string rather than
    /// failing the parse or silently becoming a default -- a new category
    /// kind in a server upgrade must not empty the sidebar.
    #[test]
    fn unknown_kinds_and_sortings_survive_as_their_raw_string() {
        let json = r#"{"id":"x","type":"shared_channels","sorting":"by_vibes"}"#;
        let category: ChannelCategory = serde_json::from_str(json).expect("parses");
        assert_eq!(category.kind, CategoryKind::Other("shared_channels".into()));
        assert_eq!(category.sorting, CategorySorting::Other("by_vibes".into()));
    }

    /// Round-trip: what we write is what the server would have sent, which is
    /// what makes this type safe to send back when reordering lands.
    #[test]
    fn every_kind_and_sorting_round_trips() {
        let kinds = [
            CategoryKind::Favorites,
            CategoryKind::Channels,
            CategoryKind::DirectMessages,
            CategoryKind::Custom,
            CategoryKind::Other("shared_channels".into()),
        ];
        let sortings = [
            CategorySorting::Default,
            CategorySorting::Alpha,
            CategorySorting::Recent,
            CategorySorting::Manual,
            CategorySorting::Other("by_vibes".into()),
        ];
        for kind in kinds {
            for sorting in &sortings {
                let category = ChannelCategory {
                    kind: kind.clone(),
                    sorting: sorting.clone(),
                    ..ChannelCategory::default()
                };
                let text = serde_json::to_string(&category).expect("serializes");
                let back: ChannelCategory = serde_json::from_str(&text).expect("parses");
                assert_eq!(back, category, "{text}");
            }
        }
    }
}
