//! The client's view of one server.
//!
//! **Every mutation here is a monotonic join, never a blind assignment.** That
//! is what makes the store safe against the things the protocol actually does:
//! the server replays its dead queue after a reconnect, so the same event
//! arrives twice; a `channel_member_updated` from another device can be older
//! than what is already held; and an edit can arrive out of order with the
//! page that already contained the edited post. Under those conditions
//! "assign the newer value" is wrong and "assign whichever arrived last" is
//! worse.

use crate::sections::{SidebarSection, group};
use crate::timeline::{Merge, PageAnchor, PostKey, Timeline};
use crate::unread::{ChannelTotals, MarkUnread, MemberCounts, Threading, Unread};
use mm_api::ws::WsEvent;
use mm_types::{
    CategoryList, Channel, ChannelCategory, ChannelId, ChannelMember, ChannelStats, ChannelType,
    Post, PostId, PostList, Team, Timestamp, User, UserId,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What changed, so a GUI can invalidate only what it must.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A channel's unread or mention state moved.
    Unread(ChannelId),
    /// A post was added or updated.
    Post(PostId),
    /// A post is gone.
    PostRemoved(PostId),
    /// A user record changed.
    User(UserId),
    /// The sidebar's membership or ordering changed.
    Sidebar,
    /// Read state was pushed for a set of channels at once.
    ReadState,
}

/// One emoji's worth of reaction on a post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionSummary {
    pub emoji: String,
    pub count: i64,
    /// Whether the signed-in user is one of them, which decides whether
    /// clicking the chip adds or takes back.
    pub mine: bool,
    /// When this emoji first appeared on the post; the display order.
    pub first: Timestamp,
}

/// One row of the channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub channel: ChannelId,
    pub display_name: String,
    pub kind: ChannelType,
    /// For a one-to-one DM, the person on the other end: the sidebar draws
    /// their face where a channel would draw an icon. `None` for anything
    /// else, including a group DM, which has no single other person.
    pub avatar_user: Option<UserId>,
    pub unread: Unread,
    pub muted: bool,
}

impl SidebarRow {
    #[must_use]
    pub fn is_unread(&self) -> bool {
        self.unread.is_unread(if self.muted {
            MarkUnread::MentionsOnly
        } else {
            MarkUnread::All
        })
    }
}

/// The store.
#[derive(Debug, Clone, Default)]
pub struct Store {
    pub me: UserId,
    /// Whether collapsed reply threads are in effect. Must be set from the
    /// server config plus the user preference, never assumed.
    pub threading: Threading,
    pub users: HashMap<UserId, User>,
    pub teams: Vec<Team>,
    pub channels: HashMap<ChannelId, Channel>,
    /// My membership only. Unread never needs anybody else's, and the two
    /// arrive from different endpoints.
    pub my_member: HashMap<ChannelId, ChannelMember>,
    pub posts: HashMap<PostId, Post>,
    pub timelines: HashMap<ChannelId, Timeline>,
    /// The server's grouping of the sidebar, already in stacking order.
    /// Empty until the first sync, which is why `sidebar` still exists.
    pub categories: Vec<ChannelCategory>,
    /// Member counts, fetched per channel when it is opened.
    pub stats: HashMap<ChannelId, ChannelStats>,
    /// Who reacted with what, and when they first did. Keyed separately from
    /// the post because reactions do not move a post's `update_at`, so they
    /// cannot ride the newest-wins join that posts use.
    pub reactions: HashMap<PostId, BTreeMap<(String, UserId), Timestamp>>,
}

impl Store {
    #[must_use]
    pub fn new(me: UserId, threading: Threading) -> Self {
        Self {
            me,
            threading,
            ..Self::default()
        }
    }

    // ------------------------------------------------------------ REST

    pub fn apply_teams(&mut self, teams: Vec<Team>) {
        self.teams = teams;
    }

    /// Replace the sidebar grouping, in the server's stacking order.
    ///
    /// A blind replace rather than the monotonic join the rest of the store
    /// uses: the server sends the whole list every time and carries no
    /// version on it, so there is nothing to compare and a removed category
    /// has to actually disappear.
    pub fn apply_categories(&mut self, list: &CategoryList) -> Vec<Change> {
        self.categories = list.in_order().into_iter().cloned().collect();
        vec![Change::Sidebar]
    }

    /// The sidebar as sections, if the server has told us how it is grouped.
    ///
    /// Empty before the first sync completes, and the caller falls back to
    /// the flat `sidebar` then -- a sidebar that renders nothing until a
    /// second request returns would show an empty window on every start.
    #[must_use]
    pub fn sidebar_sections(&self) -> Vec<SidebarSection> {
        let recency: HashMap<ChannelId, i64> = self
            .channels
            .iter()
            .map(|(id, c)| (id.clone(), c.last_post_at.millis()))
            .collect();
        group(self.sidebar(), &self.categories, &recency)
    }

    /// Record a channel's member count.
    pub fn apply_stats(&mut self, stats: ChannelStats) -> Vec<Change> {
        let channel = stats.channel_id.clone();
        self.stats.insert(channel.clone(), stats);
        vec![Change::Unread(channel)]
    }

    /// What the channel header says: the channel's name, and how many people
    /// are in it once that is known.
    ///
    /// Returns the raw id for a channel we have not loaded, for the same
    /// reason `channel_label` does: showing something beats showing a blank
    /// header while the first sync is still in flight.
    #[must_use]
    pub fn channel_heading(&self, channel: &ChannelId) -> (String, Option<i64>) {
        let name = self
            .channels
            .get(channel)
            .map_or_else(|| channel.to_string(), |c| self.channel_label(c));
        (name, self.stats.get(channel).map(|s| s.member_count))
    }

    /// A post's reactions, grouped for display.
    ///
    /// Ordered by when each emoji was *first* used on the post, which is the
    /// order Mattermost shows and the only one that does not reshuffle the
    /// row every time somebody joins an existing reaction.
    #[must_use]
    pub fn reactions_for(&self, post: &PostId) -> Vec<ReactionSummary> {
        let Some(held) = self.reactions.get(post) else {
            return Vec::new();
        };
        let mut grouped: BTreeMap<&str, ReactionSummary> = BTreeMap::new();
        for ((emoji, user), at) in held {
            let entry = grouped
                .entry(emoji.as_str())
                .or_insert_with(|| ReactionSummary {
                    emoji: emoji.clone(),
                    count: 0,
                    mine: false,
                    first: *at,
                });
            entry.count += 1;
            entry.mine |= *user == self.me;
            entry.first = entry.first.min(*at);
        }
        let mut out: Vec<ReactionSummary> = grouped.into_values().collect();
        out.sort_by(|a, b| a.first.cmp(&b.first).then_with(|| a.emoji.cmp(&b.emoji)));
        out
    }

    pub fn apply_users(&mut self, users: Vec<User>) -> Vec<Change> {
        users
            .into_iter()
            .map(|u| {
                let id = u.id.clone();
                self.users.insert(id.clone(), u);
                Change::User(id)
            })
            .collect()
    }

    pub fn apply_channels(&mut self, channels: Vec<Channel>) -> Vec<Change> {
        for channel in channels {
            self.apply_channel(channel);
        }
        vec![Change::Sidebar]
    }

    /// Insert or update a channel.
    ///
    /// `total_msg_count` only ever grows, so an older payload must not drag it
    /// back: doing so would make a channel the user has unread messages in
    /// look read until the next update arrives.
    pub fn apply_channel(&mut self, incoming: Channel) {
        match self.channels.get_mut(&incoming.id) {
            Some(held) if held.update_at > incoming.update_at => {
                held.total_msg_count = held.total_msg_count.max(incoming.total_msg_count);
                held.total_msg_count_root =
                    held.total_msg_count_root.max(incoming.total_msg_count_root);
            }
            _ => {
                let id = incoming.id.clone();
                self.channels.insert(id, incoming);
            }
        }
    }

    pub fn apply_members(&mut self, members: Vec<ChannelMember>) -> Vec<Change> {
        members
            .into_iter()
            .filter_map(|m| self.apply_member(m))
            .collect()
    }

    /// Insert or update my membership of a channel.
    ///
    /// The counters only move forward, except through an explicit mark-unread.
    /// Mentions are taken from the incoming record only when it is at least as
    /// fresh as what is held — otherwise a `channel_member_updated` that was
    /// generated before a mention we already know about would clear the badge.
    pub fn apply_member(&mut self, incoming: ChannelMember) -> Option<Change> {
        let id = incoming.channel_id.clone();
        match self.my_member.get_mut(&id) {
            None => {
                self.my_member.insert(id.clone(), incoming);
                Some(Change::Unread(id))
            }
            Some(held) => {
                let stale = match (held.last_update_at, incoming.last_update_at) {
                    (Some(a), Some(b)) => b < a,
                    _ => false,
                };
                held.last_viewed_at = match (held.last_viewed_at, incoming.last_viewed_at) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                held.msg_count = held.msg_count.max(incoming.msg_count);
                held.msg_count_root = held.msg_count_root.max(incoming.msg_count_root);
                if !stale {
                    held.mention_count = incoming.mention_count;
                    held.mention_count_root = incoming.mention_count_root;
                    held.urgent_mention_count = incoming.urgent_mention_count;
                    held.notify_props = incoming.notify_props;
                    held.last_update_at = incoming.last_update_at.or(held.last_update_at);
                }
                Some(Change::Unread(id))
            }
        }
    }

    /// Merge a page of posts into a channel.
    ///
    /// A page that would leave a hole is refused by the timeline; the posts
    /// themselves are still stored, because a thread root pulled in by
    /// `?since=` is wanted even when it does not belong in the ordering.
    pub fn apply_post_list(
        &mut self,
        channel: &ChannelId,
        page: &PostList,
        anchor: PageAnchor,
    ) -> Vec<Change> {
        let mut changes = Vec::new();
        for post in page.posts.values() {
            if self.store_post(post.clone()) {
                changes.push(Change::Post(post.id.clone()));
            }
        }
        let timeline = self.timelines.entry(channel.clone()).or_default();
        match timeline.merge_page(page, anchor) {
            Merge::Added(_) => {
                if page.prev_post_id.is_empty() {
                    timeline.set_older_complete(true);
                }
            }
            Merge::NoChange | Merge::WouldLeaveGap => {}
        }
        changes
    }

    /// Store a post, newest-wins by `update_at`. Returns whether anything
    /// changed.
    fn store_post(&mut self, incoming: Post) -> bool {
        // Reactions are seeded whether or not the post itself is newer:
        // reacting does not move `update_at`, so a post that loses the
        // newest-wins check below may still carry the only reaction list we
        // are going to see.
        //
        // This is a replace, not a join. The server sends the live set and
        // carries no version on it, so there is nothing to compare; a
        // reaction that arrived over the WebSocket while a page was in
        // flight can be lost, and the next time the channel is opened it
        // comes back. A join instead would make a reaction removed on
        // another device permanent, which is the worse failure.
        if let Some(live) = incoming.reactions() {
            self.reactions.insert(
                incoming.id.clone(),
                live.into_iter()
                    .filter(|r| !r.is_deleted())
                    .map(|r| ((r.emoji_name, r.user_id), r.create_at))
                    .collect(),
            );
        }
        match self.posts.get(&incoming.id) {
            // An out-of-order edit must not regress a newer one, and a
            // replayed `posted` must not resurrect a deleted post.
            Some(held) if held.update_at >= incoming.update_at => false,
            _ => {
                self.posts.insert(incoming.id.clone(), incoming);
                true
            }
        }
    }

    // ------------------------------------------------------------ WebSocket

    pub fn apply_ws(&mut self, event: &WsEvent) -> Vec<Change> {
        match event {
            WsEvent::Posted { post, .. } => self.on_posted(post),
            WsEvent::PostEdited(post) => {
                if self.store_post(post.clone()) {
                    vec![Change::Post(post.id.clone())]
                } else {
                    Vec::new()
                }
            }
            WsEvent::PostDeleted(post) => self.on_deleted(post),
            WsEvent::ReactionAdded(reaction) => {
                self.reactions
                    .entry(reaction.post_id.clone())
                    .or_default()
                    .insert(
                        (reaction.emoji_name.clone(), reaction.user_id.clone()),
                        reaction.create_at,
                    );
                vec![Change::Post(reaction.post_id.clone())]
            }
            WsEvent::ReactionRemoved(reaction) => {
                if let Some(held) = self.reactions.get_mut(&reaction.post_id) {
                    held.remove(&(reaction.emoji_name.clone(), reaction.user_id.clone()));
                }
                vec![Change::Post(reaction.post_id.clone())]
            }
            WsEvent::ChannelMemberUpdated(member) => {
                self.apply_member(member.clone()).into_iter().collect()
            }
            WsEvent::MultipleChannelsViewed { channel_times } => {
                for (id, at) in channel_times {
                    self.mark_viewed(&ChannelId::new(id.clone()), Timestamp(*at));
                }
                vec![Change::ReadState]
            }
            WsEvent::ChannelViewed { channel_id } => {
                let now = self
                    .channels
                    .get(channel_id)
                    .map_or(Timestamp::ZERO, |c| c.last_post_at);
                self.mark_viewed(channel_id, now);
                vec![Change::ReadState]
            }
            WsEvent::UserUpdated(user) => {
                let id = user.id.clone();
                self.users.insert(id.clone(), user.clone());
                vec![Change::User(id)]
            }
            // Presence, typing and anything unrecognised carry no state this
            // store owns yet. Returning no changes is correct, not a stub.
            WsEvent::StatusChange { .. } | WsEvent::Typing { .. } | WsEvent::Unknown { .. } => {
                Vec::new()
            }
        }
    }

    fn on_posted(&mut self, post: &Post) -> Vec<Change> {
        let mut changes = Vec::new();
        if !self.store_post(post.clone()) {
            return changes;
        }
        changes.push(Change::Post(post.id.clone()));

        let timeline = self.timelines.entry(post.channel_id.clone()).or_default();
        timeline.insert(post);

        // The channel's own counter has to move too, or the unread arithmetic
        // (total minus seen) will not notice the new message until the next
        // channel fetch.
        if let Some(channel) = self.channels.get_mut(&post.channel_id) {
            channel.total_msg_count += 1;
            if !post.is_reply() {
                channel.total_msg_count_root += 1;
            }
            channel.last_post_at = channel.last_post_at.max(post.create_at);
            changes.push(Change::Unread(post.channel_id.clone()));
        }
        changes
    }

    fn on_deleted(&mut self, post: &Post) -> Vec<Change> {
        // A tombstone, not a removal: a late `posted` for the same id must not
        // resurrect it, and a permalink to it should say "message deleted"
        // rather than render nothing.
        let mut tomb = post.clone();
        if tomb.delete_at.is_zero() {
            tomb.delete_at = tomb.update_at.max(Timestamp(1));
        }
        self.posts.insert(tomb.id.clone(), tomb);
        if let Some(timeline) = self.timelines.get_mut(&post.channel_id) {
            timeline.remove(post);
        }
        vec![Change::PostRemoved(post.id.clone())]
    }

    // ------------------------------------------------------------ read state

    /// Mark a channel read locally, as the GUI does the moment it is opened.
    ///
    /// Optimistic on purpose: waiting for the server round trip makes the
    /// badge lag behind the click. The joins keep it safe — a later
    /// `multiple_channels_viewed` carrying an older timestamp cannot undo it.
    pub fn mark_read_locally(&mut self, channel: &ChannelId) -> Vec<Change> {
        let totals = self.totals(channel);
        if let Some(member) = self.my_member.get_mut(channel) {
            member.msg_count = member.msg_count.max(totals.total_msg_count);
            member.msg_count_root = member.msg_count_root.max(totals.total_msg_count_root);
            member.mention_count = 0;
            member.mention_count_root = 0;
            member.urgent_mention_count = 0;
        }
        vec![Change::Unread(channel.clone())]
    }

    fn mark_viewed(&mut self, channel: &ChannelId, at: Timestamp) {
        let totals = self.totals(channel);
        if let Some(member) = self.my_member.get_mut(channel) {
            member.last_viewed_at = Some(match member.last_viewed_at {
                Some(held) => held.max(at),
                None => at,
            });
            member.msg_count = member.msg_count.max(totals.total_msg_count);
            member.msg_count_root = member.msg_count_root.max(totals.total_msg_count_root);
            member.mention_count = 0;
            member.mention_count_root = 0;
        }
    }

    fn totals(&self, channel: &ChannelId) -> ChannelTotals {
        self.channels
            .get(channel)
            .map(|c| ChannelTotals {
                total_msg_count: c.total_msg_count,
                total_msg_count_root: c.total_msg_count_root,
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ queries

    #[must_use]
    pub fn unread(&self, channel: &ChannelId) -> Unread {
        let member = self.my_member.get(channel);
        let counts = member.map_or_else(MemberCounts::default, |m| MemberCounts {
            msg_count: m.msg_count,
            msg_count_root: m.msg_count_root,
            mention_count: m.mention_count,
            mention_count_root: m.mention_count_root,
        });
        crate::unread::unread(self.totals(channel), counts, self.threading)
    }

    #[must_use]
    pub fn is_muted(&self, channel: &ChannelId) -> bool {
        self.my_member
            .get(channel)
            .is_some_and(ChannelMember::is_muted)
    }

    /// Total mentions across every channel — what the tray badge shows.
    ///
    /// Muted channels are included, because muting suppresses the unread
    /// marker and not mentions.
    #[must_use]
    pub fn total_mentions(&self) -> i64 {
        self.my_member
            .keys()
            .map(|id| self.unread(id).mentions)
            .sum()
    }

    #[must_use]
    pub fn any_unread(&self) -> bool {
        self.sidebar().iter().any(SidebarRow::is_unread)
    }

    /// What to call a channel.
    ///
    /// A DM's `name` is the two user ids joined by `__` and its `display_name`
    /// is empty, so the label has to come from the roster. Falling back to the
    /// raw id is deliberate: showing something is better than showing a blank
    /// row while the roster is still loading.
    #[must_use]
    pub fn channel_label(&self, channel: &Channel) -> String {
        if channel.kind != ChannelType::Direct {
            if channel.display_name.is_empty() {
                return channel.name.clone();
            }
            return channel.display_name.clone();
        }
        let other = channel
            .name
            .split("__")
            .find(|part| *part != self.me.as_str())
            .unwrap_or_default();
        self.users
            .get(&UserId::new(other))
            .map_or_else(|| other.to_owned(), |u| u.display_name().to_owned())
    }

    /// Tell apart two rows that would otherwise read identically.
    ///
    /// Two colleagues called "Alex" are two rows called "Alex", and an
    /// unread badge on one of them tells the user nothing. Mattermost
    /// appends the username; a channel of the same name in two teams is the
    /// same problem with a different answer, so it gets the team.
    ///
    /// Only duplicates are touched. Adding "@alex" to every DM would be
    /// noise on the ninety-nine rows that were already unambiguous.
    fn disambiguate(&self, rows: &mut [SidebarRow]) {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for row in rows.iter() {
            *seen.entry(row.display_name.as_str()).or_default() += 1;
        }
        let duplicated: BTreeSet<String> = seen
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(name, _)| name.to_owned())
            .collect();
        if duplicated.is_empty() {
            return;
        }
        for row in rows.iter_mut() {
            if !duplicated.contains(&row.display_name) {
                continue;
            }
            if let Some(detail) = self.distinguishing_detail(&row.channel) {
                row.display_name = format!("{} ({detail})", row.display_name);
            }
        }
    }

    /// The other person in a one-to-one DM.
    fn other_party(&self, channel: &Channel) -> Option<UserId> {
        if channel.kind != ChannelType::Direct {
            return None;
        }
        channel
            .name
            .split("__")
            .find(|part| *part != self.me.as_str())
            .map(UserId::new)
    }

    /// What separates this channel from another of the same name: the other
    /// person's username for a DM, the team for anything else.
    fn distinguishing_detail(&self, channel: &ChannelId) -> Option<String> {
        let channel = self.channels.get(channel)?;
        if channel.kind == ChannelType::Direct {
            let other = channel
                .name
                .split("__")
                .find(|part| *part != self.me.as_str())?;
            let user = self.users.get(&UserId::new(other))?;
            // A username is unique on a server by construction, so this
            // cannot itself collide.
            return Some(format!("@{}", user.username));
        }
        let team = self.teams.iter().find(|t| t.id == channel.team_id)?;
        // A group DM has no team, and two of them with the same set of
        // display names are genuinely indistinguishable by name alone.
        (!team.display_name.is_empty()).then(|| team.display_name.clone())
    }

    /// The channel list, deterministically ordered: unread first, then by
    /// label. Iteration order of a `HashMap` must never reach the UI.
    #[must_use]
    pub fn sidebar(&self) -> Vec<SidebarRow> {
        let mut rows: Vec<SidebarRow> = self
            .my_member
            .keys()
            .filter_map(|id| self.channels.get(id))
            .filter(|c| !c.is_deleted())
            .map(|c| SidebarRow {
                channel: c.id.clone(),
                display_name: self.channel_label(c),
                kind: c.kind.clone(),
                avatar_user: self.other_party(c),
                unread: self.unread(&c.id),
                muted: self.is_muted(&c.id),
            })
            .collect();
        self.disambiguate(&mut rows);
        rows.sort_by(|a, b| {
            b.is_unread()
                .cmp(&a.is_unread())
                .then_with(|| a.display_name.cmp(&b.display_name))
                .then_with(|| a.channel.cmp(&b.channel))
        });
        rows
    }

    /// A channel's posts, oldest first, skipping tombstones.
    ///
    /// **Under collapsed reply threads, replies are excluded.** That is not a
    /// display preference — it is what CRT means. The server maintains
    /// root-only counters precisely so a reply does not make a channel unread,
    /// so showing replies in the channel makes the message list disagree with
    /// the unread badge beside it, and turns a threaded conversation into a
    /// flat firehose.
    #[must_use]
    pub fn channel_posts(&self, channel: &ChannelId) -> Vec<&Post> {
        let collapsed = self.threading == Threading::Collapsed;
        self.timelines
            .get(channel)
            .into_iter()
            .flat_map(Timeline::keys)
            .filter_map(|key: &PostKey| self.posts.get(&key.id))
            .filter(|p| !p.is_deleted())
            .filter(|p| !collapsed || !p.is_reply())
            .collect()
    }

    /// How many replies a root post has, counting what is actually held.
    ///
    /// The server's own `reply_count` is preferred when it is set, because it
    /// counts replies this client has never fetched. Falling back to a local
    /// count keeps the number right for a thread built up live over the
    /// WebSocket, where the root post was fetched before the replies existed.
    #[must_use]
    pub fn reply_count(&self, root: &PostId) -> i64 {
        let stated = self.posts.get(root).map_or(0, |p| p.reply_count);
        let held = self
            .posts
            .values()
            .filter(|p| !p.is_deleted() && p.root_id == *root)
            .count();
        stated.max(i64::try_from(held).unwrap_or(i64::MAX))
    }
}

#[cfg(test)]
#[path = "tests/store.rs"]
mod tests;
