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
    NameFormat, Post, PostId, PostList, Team, Timestamp, User, UserId,
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
    /// The followed-thread list changed.
    Threads,
    /// The unsent-message list changed.
    Drafts,
    /// An answer list -- search, pinned, saved -- changed.
    Found,
    /// The server's custom emoji list changed, which can affect any
    /// message and every reaction on screen.
    Emoji,
}

/// Which answer list is being asked for.
///
/// One type for the three because they are the same thing from the
/// store's side: a set of posts in an order the server chose, which is
/// not the order of any channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Search,
    Saved,
    Pinned(ChannelId),
}

/// Where a dragged channel lands in its new category.
///
/// Expressed against a neighbour rather than as an index, because the
/// sidebar can be showing fewer rows than the category holds -- a
/// folded category shows none, and the unread filter hides the rest --
/// so the position on screen is not the position in `channel_ids`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// At the top, which is what dropping on the heading means.
    Front,
    /// Just above this channel.
    Before(ChannelId),
}

/// What the window's title bar says about the open channel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heading {
    pub name: String,
    /// Absent for a one-to-one DM, where it would say nothing.
    pub members: Option<i64>,
    /// The channel header's first line, with its markdown rendered away.
    pub topic: String,
}

/// How many people are named in a group DM's title.
///
/// The server builds that title by joining the participants' usernames
/// with ", ", and a username cannot contain a comma, so counting them is
/// exact rather than a guess.
fn participants(display_name: &str) -> Option<i64> {
    let named = display_name
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .count();
    (named > 0).then(|| i64::try_from(named).unwrap_or(i64::MAX))
}

/// The three names that mean a group rather than a person, and what
/// each of them does.
///
/// Not users, so they are kept apart from `mention_candidates` rather
/// than faked into the roster: they have no avatar, no presence and no
/// id, and every one of those would have to be invented. Mattermost
/// draws them under their own heading for the same reason.
pub const SPECIAL_MENTIONS: &[(&str, &str)] = &[
    ("all", "Notifies everyone in this channel"),
    ("channel", "Notifies everyone in this channel"),
    ("here", "Notifies everyone online in this channel"),
];

/// Whether a name after an `@` means a group rather than a person.
#[must_use]
pub fn is_special_mention(name: &str) -> bool {
    SPECIAL_MENTIONS.iter().any(|(word, _)| *word == name)
}

/// One name the `@` popup is offering, and whether they are in the channel.
///
/// The flag is what lets the popup draw Mattermost's two headings:
/// everyone in the conversation first, then anyone else who matches.
/// Mentioning a non-member is legal -- the server says so in the channel
/// afterwards -- so they are offered, just never first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mention<'a> {
    pub user: &'a User,
    pub in_channel: bool,
}

/// How well a person answers `@prefix`: 0 is best, `None` is no match.
///
/// The username leads because it is what the mention actually becomes;
/// the human name matters because it is what the reader is thinking of.
fn mention_rank(user: &User, wanted: &str) -> Option<u8> {
    if wanted.is_empty() {
        return Some(0);
    }
    let username = user.username.to_lowercase();
    if username.starts_with(wanted) {
        return Some(0);
    }
    let human = [
        user.first_name.as_str(),
        user.last_name.as_str(),
        user.nickname.as_str(),
    ];
    if human
        .iter()
        .any(|part| part.to_lowercase().starts_with(wanted))
    {
        return Some(1);
    }
    if username.contains(wanted) {
        return Some(2);
    }
    None
}

/// How well a channel's name answers what was typed: 0 is best.
///
/// `None` means it does not match at all. A word start counts because
/// "env" should find "Stage-environment", and the separators are the
/// ones Mattermost names channels with.
fn rank(name: &str, wanted: &str) -> Option<u8> {
    if wanted.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name.starts_with(wanted) {
        return Some(0);
    }
    let at = name.find(wanted)?;
    let word_start = name[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c == ' ' || c == '-' || c == '_' || c == '.');
    Some(if word_start { 1 } else { 2 })
}

/// How much of the Threads view wants attention, account-wide.
///
/// The server's own totals rather than a count of the page in hand: one
/// page of threads is not all of them, and a badge that counted the page
/// would stop rising at twenty-five.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadTotals {
    pub unread_threads: i64,
    pub unread_mentions: i64,
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
    /// Something half-typed in it, which the row marks with a pencil
    /// the way the webapp does: a draft is a thing to come back to.
    pub has_draft: bool,
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
    /// The reader's own username, for telling a mention of them from
    /// one of anybody else. Held apart from `users`, which is a roster
    /// of the people on screen and need not hold the reader at all.
    pub my_username: String,
    /// When the reader's own timed do-not-disturb ends, in seconds since
    /// the epoch; `None` for no end, or no do-not-disturb.
    pub my_dnd_until: Option<i64>,
    /// Whether collapsed reply threads are in effect. Must be set from the
    /// server config plus the user preference, never assumed.
    pub threading: Threading,
    /// How long after posting a message may still be edited; `None` is
    /// no limit. From the server config, through `settings::edit_limit`.
    pub edit_limit: Option<std::time::Duration>,
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
    /// Which name to show for a person, from the server's setting and the
    /// user's preference. Never assumed: the default is `username`, and
    /// guessing otherwise renames everyone in the window.
    pub name_format: NameFormat,
    /// The reader's preferences, flat and keyed `(category, name)` the
    /// way the server stores them. Flat on purpose: Mattermost adds
    /// preference categories constantly, and a struct per category would
    /// need changing for each one.
    pub prefs: BTreeMap<(String, String), String>,
    /// Member counts, fetched per channel when it is opened.
    pub stats: HashMap<ChannelId, ChannelStats>,
    /// Who belongs to a channel, from the roster fetched when it opened.
    ///
    /// Kept per channel because `users` is everyone this client has ever
    /// heard of -- post authors, DM partners, and the roster of every
    /// channel visited so far -- so offering all of it to `@` suggests
    /// people who cannot read the message. Absent until the channel has
    /// been opened once, which is why the ordering below falls back
    /// rather than showing nothing.
    pub channel_members: HashMap<ChannelId, BTreeSet<UserId>>,
    /// Who is at their desk. Fetched for the people on screen and kept
    /// current by `status_change` over the WebSocket. Absent means
    /// "never asked", which draws the same as offline but is not the
    /// same thing, so it is not defaulted into the map.
    pub statuses: HashMap<UserId, mm_types::Presence>,
    /// The server's own custom emoji, by name.
    ///
    /// Name to id, never the other way round: a reaction and a typed
    /// `:shortcode:` both carry the name, and the id exists only to
    /// build the image URL. Empty means the list has not been fetched,
    /// which draws the same as a server with none -- shortcodes --
    /// which is the right fallback either way.
    pub custom_emoji: BTreeMap<String, mm_types::EmojiId>,
    /// The threads this reader follows, as the server last listed them,
    /// and the account-wide totals that go on the sidebar badge. Replaced
    /// wholesale rather than merged: unlike a channel's posts this is a
    /// complete answer to one question, and a thread the reader has
    /// stopped following has to be able to leave the list.
    pub threads: Vec<mm_types::UserThread>,
    pub thread_totals: ThreadTotals,
    /// What the last search answered, in the server's order, and what
    /// was asked. Kept apart from the timelines on purpose: these posts
    /// are adjacent for reasons that have nothing to do with time.
    pub search_results: Vec<PostId>,
    pub search_terms: String,
    /// What is pinned, per channel.
    pub pinned: HashMap<ChannelId, Vec<PostId>>,
    /// What this reader has saved, across every channel.
    pub saved: Vec<PostId>,
    /// Unsent messages, from the server, so what was typed in another
    /// client is waiting here too. Replaced wholesale for the same reason.
    pub drafts: Vec<mm_types::Draft>,
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

    /// Take in the followed threads, and the totals behind the badge.
    ///
    /// The root posts are folded into the post store on the way through,
    /// so the view can be drawn from the same rows a channel is drawn
    /// from -- and so a thread opened from the list has its root already.
    pub fn apply_threads(&mut self, list: &mm_types::ThreadList) -> Vec<Change> {
        for thread in &list.threads {
            if let Some(post) = &thread.post {
                self.store_post(post.clone());
            }
        }
        self.threads.clone_from(&list.threads);
        self.thread_totals = ThreadTotals {
            unread_threads: list.total_unread_threads,
            unread_mentions: list.total_unread_mentions,
        };
        vec![Change::Sidebar, Change::Threads]
    }

    /// The followed threads worth drawing, the ones with new replies
    /// first and then by when they were last replied to.
    ///
    /// Record that a followed thread has been read.
    ///
    /// Applied here as well as sent. The server announces this as a
    /// `thread_read_changed`, which this client does not decode, so
    /// without it the row stays bold until the next sync -- and a
    /// "mark read" that leaves the row looking unread reads as a
    /// click that missed.
    pub fn read_thread_locally(&mut self, root: &PostId) -> Vec<Change> {
        let Some(thread) = self.threads.iter_mut().find(|held| held.id == *root) else {
            return Vec::new();
        };
        if !thread.is_unread() {
            return Vec::new();
        }
        // The account-wide totals behind the sidebar badge are the
        // server's, so they are adjusted rather than recounted: this
        // page is not all the threads there are.
        self.thread_totals.unread_threads = self.thread_totals.unread_threads.saturating_sub(1);
        self.thread_totals.unread_mentions = self
            .thread_totals
            .unread_mentions
            .saturating_sub(thread.unread_mentions);
        thread.unread_replies = 0;
        thread.unread_mentions = 0;
        thread.last_viewed_at = thread.last_reply_at;
        vec![Change::Threads, Change::Sidebar]
    }

    /// Record that every followed thread has been read.
    pub fn read_all_threads_locally(&mut self) -> Vec<Change> {
        let mut moved = false;
        for thread in &mut self.threads {
            if thread.is_unread() {
                moved = true;
                thread.unread_replies = 0;
                thread.unread_mentions = 0;
                thread.last_viewed_at = thread.last_reply_at;
            }
        }
        // All of them, not just the page in hand: that is what the
        // server was asked to do.
        moved |= self.thread_totals.unread_threads != 0;
        self.thread_totals = ThreadTotals::default();
        if moved {
            vec![Change::Threads, Change::Sidebar]
        } else {
            Vec::new()
        }
    }

    /// Mattermost sorts the view by last reply alone. Unread first is a
    /// deliberate difference and the same rule the sidebar already uses:
    /// the reason to open this view is to find what has moved.
    #[must_use]
    pub fn followed_threads(&self) -> Vec<&mm_types::UserThread> {
        let mut out: Vec<&mm_types::UserThread> = self
            .threads
            .iter()
            // A thread whose root we were not given cannot be drawn, and
            // one the server has deleted should not be.
            .filter(|t| t.post.is_some() && t.delete_at.millis() == 0)
            .collect();
        out.sort_by(|a, b| {
            b.is_unread()
                .cmp(&a.is_unread())
                .then_with(|| b.last_reply_at.millis().cmp(&a.last_reply_at.millis()))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// Take in the unsent messages.
    pub fn apply_drafts(&mut self, drafts: &[mm_types::Draft]) -> Vec<Change> {
        self.drafts = drafts.to_vec();
        vec![Change::Sidebar, Change::Drafts]
    }

    /// Record a draft this client has just written.
    ///
    /// Applied here as well as sent, like a folded category: the Drafts
    /// view and the composer both read the store, and waiting for the
    /// round trip would mean switching channels and back lost what was
    /// typed. An empty one is a deletion, because that is what the
    /// server does with it.
    pub fn set_draft_locally(&mut self, draft: mm_types::Draft) -> Vec<Change> {
        let same = |held: &mm_types::Draft| {
            held.channel_id == draft.channel_id && held.root_id == draft.root_id
        };
        if draft.is_empty() {
            let before = self.drafts.len();
            self.drafts.retain(|held| !same(held));
            if self.drafts.len() == before {
                return Vec::new();
            }
        } else if let Some(held) = self.drafts.iter_mut().find(|held| same(held)) {
            // By content, not by the whole record: the window stamps
            // `update_at` from the clock, so comparing everything
            // would make every idle tick a change and every change a
            // request.
            if held.message == draft.message && held.file_ids == draft.file_ids {
                return Vec::new();
            }
            *held = draft;
        } else {
            self.drafts.push(draft);
        }
        vec![Change::Sidebar, Change::Drafts]
    }

    /// What is waiting in a channel's composer, or in a thread's when
    /// `root` is set.
    #[must_use]
    pub fn draft_for(
        &self,
        channel: &ChannelId,
        root: Option<&PostId>,
    ) -> Option<&mm_types::Draft> {
        let wanted = root.map_or("", PostId::as_str);
        self.drafts
            .iter()
            .find(|held| held.channel_id == *channel && held.root_id.as_str() == wanted)
    }

    /// The drafts worth drawing, most recently touched first.
    ///
    /// An empty one is skipped rather than drawn as a blank row: the
    /// server keeps a draft whose text was deleted while it still has
    /// files, and drops it only later.
    #[must_use]
    pub fn pending_drafts(&self) -> Vec<&mm_types::Draft> {
        let mut out: Vec<&mm_types::Draft> = self
            .drafts
            .iter()
            .filter(|d| !d.is_empty() && d.delete_at.millis() == 0)
            .collect();
        out.sort_by(|a, b| {
            b.update_at
                .millis()
                .cmp(&a.update_at.millis())
                .then_with(|| a.channel_id.cmp(&b.channel_id))
        });
        out
    }

    /// The channels matching what somebody typed into the switcher,
    /// best match first.
    ///
    /// Three tiers, because they are what a reader means by "better":
    /// a name that *starts* with what they typed, then one where a word
    /// does, then one that merely contains it. Within a tier the unread
    /// channels come first -- somebody reaching for the switcher is
    /// usually going somewhere that has moved -- and then by name, so
    /// the list is stable while they keep typing.
    ///
    /// An empty query is the sidebar as it stands, which is what the
    /// switcher shows before the first keystroke.
    #[must_use]
    pub fn find_channels(&self, query: &str, limit: usize) -> Vec<SidebarRow> {
        let wanted = query.trim().to_lowercase();
        let mut rows: Vec<(u8, SidebarRow)> = self
            .sidebar()
            .into_iter()
            .filter_map(|row| rank(&row.display_name, &wanted).map(|tier| (tier, row)))
            .collect();
        // `sidebar` is already unread-first and then alphabetical, and
        // `sort_by` is stable, so ranking by tier alone keeps both.
        rows.sort_by_key(|(tier, _)| *tier);
        rows.into_iter().map(|(_, row)| row).take(limit).collect()
    }

    /// The people who might be meant by `@prefix`, best first.
    ///
    /// Only people this client has heard of: the authors of the open
    /// channel's messages, DM partners, thread participants. Mattermost
    /// asks the server, which knows every member of the channel; this
    /// does not, so somebody who has not spoken recently will not be
    /// offered. In a channel being read that is nearly always the set
    /// worth offering, and it costs no request per keystroke.
    ///
    /// Ranked the way a reader means it: the username they are typing
    /// first, then a name that begins with it, then anything containing
    /// it. Deactivated people are left out -- mentioning one does
    /// nothing -- and so are bots, which do not read their mentions.
    #[must_use]
    pub fn mention_candidates(
        &self,
        channel: Option<&ChannelId>,
        prefix: &str,
        limit: usize,
    ) -> Vec<Mention<'_>> {
        let wanted = prefix.trim().to_lowercase();
        let members = channel.and_then(|c| self.channel_members.get(c));
        let spoke = channel.map(|c| self.last_spoke(c)).unwrap_or_default();
        // Four keys, in the order the reader thinks: the people here
        // before anyone else, the better match before the worse one, the
        // person who just said something before the one who never has,
        // and the alphabet only to break a genuine tie.
        let mut found: Vec<(u8, u8, std::cmp::Reverse<i64>, &User)> = self
            .users
            .values()
            .filter(|u| u.delete_at.is_zero() && !u.is_bot && !u.username.is_empty())
            .filter_map(|u| {
                let tier = mention_rank(u, &wanted)?;
                let here = u8::from(!members.is_some_and(|m| m.contains(&u.id)));
                let said = spoke.get(&u.id).copied().unwrap_or(0);
                Some((here, tier, std::cmp::Reverse(said), u))
            })
            .collect();
        found.sort_by(
            |(here, tier, said, a), (other_here, other_tier, other_said, b)| {
                here.cmp(other_here)
                    .then_with(|| tier.cmp(other_tier))
                    .then_with(|| said.cmp(other_said))
                    .then_with(|| a.username.cmp(&b.username))
            },
        );
        found
            .into_iter()
            .map(|(here, _, _, user)| Mention {
                user,
                in_channel: here == 0,
            })
            .take(limit)
            .collect()
    }

    /// People who could be added to a channel, matching what is typed.
    ///
    /// The mirror of `mention_candidates`: that one puts the people who
    /// are already here first, and this one leaves them out entirely.
    /// Deactivated accounts and bots are not offered -- the server
    /// accepts both and neither can read anything -- and nor is the
    /// reader, who is already in the channel by definition of being able
    /// to open its menu.
    ///
    /// Ranked by the same `mention_rank`, so "ada" offers the same person
    /// here as it does after an `@`.
    #[must_use]
    pub fn not_in_channel(&self, channel: &ChannelId, typed: &str, limit: usize) -> Vec<User> {
        let wanted = typed.trim().to_lowercase();
        if wanted.is_empty() {
            return Vec::new();
        }
        let members = self.channel_members.get(channel);
        let mut found: Vec<(u8, &User)> = self
            .users
            .values()
            .filter(|u| u.delete_at.is_zero() && !u.is_bot && !u.username.is_empty())
            .filter(|u| u.id != self.me)
            .filter(|u| !members.is_some_and(|m| m.contains(&u.id)))
            .filter_map(|u| Some((mention_rank(u, &wanted)?, u)))
            .collect();
        found.sort_by(|(tier, a), (other_tier, b)| {
            tier.cmp(other_tier)
                .then_with(|| a.username.cmp(&b.username))
        });
        found
            .into_iter()
            .map(|(_, user)| user.clone())
            .take(limit)
            .collect()
    }

    /// When each person last said anything in a channel.
    ///
    /// Replies count, unlike `channel_posts`: someone answering in a
    /// thread is still part of the conversation, and is exactly who the
    /// reader is about to mention.
    fn last_spoke(&self, channel: &ChannelId) -> HashMap<&UserId, i64> {
        let mut latest: HashMap<&UserId, i64> = HashMap::new();
        for post in self
            .timelines
            .get(channel)
            .into_iter()
            .flat_map(Timeline::keys)
            .filter_map(|key: &PostKey| self.posts.get(&key.id))
        {
            let at = post.create_at.millis();
            let seen = latest.entry(&post.user_id).or_insert(at);
            *seen = (*seen).max(at);
        }
        latest
    }

    /// Record a batch of presences, as the status endpoint answered.
    pub fn apply_statuses(&mut self, statuses: &[mm_types::Status]) -> Vec<Change> {
        statuses
            .iter()
            .filter_map(|status| {
                let presence = status.presence();
                if status.user_id == self.me {
                    self.my_dnd_until = (presence == mm_types::Presence::Dnd
                        && status.dnd_end_time > 0)
                        .then_some(status.dnd_end_time);
                }
                let known = self.statuses.insert(status.user_id.clone(), presence);
                (known != Some(presence)).then(|| Change::User(status.user_id.clone()))
            })
            .collect()
    }

    /// Record a page of the server's custom emoji.
    ///
    /// Deleted ones are dropped rather than stored: the list can be
    /// cached, and an emoji that has been removed would draw as a
    /// broken image where its shortcode at least says what was meant.
    pub fn apply_custom_emoji(&mut self, emoji: &[mm_types::CustomEmoji]) -> Vec<Change> {
        let mut moved = false;
        for one in emoji {
            if one.is_deleted() || one.name.is_empty() {
                moved |= self.custom_emoji.remove(&one.name).is_some();
                continue;
            }
            let known = self.custom_emoji.insert(one.name.clone(), one.id.clone());
            moved |= known.as_ref() != Some(&one.id);
        }
        // One change for the batch. Every message on screen may hold one
        // of these, so the answer to "what moved" is "possibly
        // everything", and naming each post would be a longer list than
        // the redraw it asks for.
        if moved {
            vec![Change::Emoji]
        } else {
            Vec::new()
        }
    }

    /// The id behind a shortcode, if the server has an emoji by that
    /// name. `None` is the ordinary case: most shortcodes are standard.
    #[must_use]
    pub fn custom_emoji(&self, name: &str) -> Option<&mm_types::EmojiId> {
        self.custom_emoji.get(name)
    }

    /// Custom emoji whose name matches what has been typed, for the
    /// picker and the composer's `:` completion.
    ///
    /// Prefix matches first and then substring, which is the order
    /// `mm_md::search` uses for the standard table -- the two lists are
    /// shown together and a reader cannot tell which is which.
    #[must_use]
    pub fn custom_emoji_matching(&self, query: &str, limit: usize) -> Vec<String> {
        let query = query.to_ascii_lowercase();
        let mut prefix = Vec::new();
        let mut rest = Vec::new();
        for name in self.custom_emoji.keys() {
            if name.starts_with(&query) {
                prefix.push(name.clone());
            } else if name.contains(&query) {
                rest.push(name.clone());
            }
        }
        prefix.append(&mut rest);
        prefix.truncate(limit);
        prefix
    }

    /// Where somebody is, as far as this client knows.
    #[must_use]
    pub fn presence(&self, user: &UserId) -> mm_types::Presence {
        self.statuses.get(user).copied().unwrap_or_default()
    }

    /// The channel with this URL name, which is what a
    /// `mattermost://` link and a permalink both carry.
    ///
    /// `name`, not `display_name`: the URL form is lowercase and
    /// hyphenated and is the only one a link ever contains, while the
    /// display name is whatever somebody typed and may be in Russian.
    #[must_use]
    pub fn channel_named(&self, name: &str) -> Option<&Channel> {
        self.my_member
            .keys()
            .filter_map(|id| self.channels.get(id))
            .find(|channel| channel.name == name)
    }

    /// Who is in a channel, as people rather than ids, ordered by name.
    ///
    /// Empty until the channel has been opened once, which is when the
    /// roster is fetched -- not the same as nobody being in it, and the
    /// window says so.
    ///
    /// People who have left the server are not listed. The roster
    /// endpoint still returns them; the member count beside the
    /// channel's name does not, and a list of thirty-five under a
    /// heading saying twenty-four is a window disagreeing with itself.
    #[must_use]
    pub fn channel_roster(&self, channel: &ChannelId) -> Vec<&User> {
        let Some(ids) = self.channel_members.get(channel) else {
            return Vec::new();
        };
        let mut people: Vec<&User> = ids
            .iter()
            .filter_map(|id| self.users.get(id))
            .filter(|user| !user.is_deactivated())
            .collect();
        people.sort_by(|a, b| a.username.cmp(&b.username));
        people
    }

    /// Record who is in a channel, and the people themselves.
    /// Add people to a channel's roster without replacing it.
    ///
    /// For an answer that is deliberately partial -- the first few
    /// names matching what is being typed after an `@`. Replacing the
    /// roster with those would shrink the channel to whoever's name
    /// happens to start with "al".
    pub fn extend_channel_roster(&mut self, channel: &ChannelId, users: Vec<User>) -> Vec<Change> {
        self.channel_members
            .entry(channel.clone())
            .or_default()
            .extend(users.iter().map(|u| u.id.clone()));
        self.apply_users(users)
    }

    pub fn apply_channel_roster(&mut self, channel: &ChannelId, users: Vec<User>) -> Vec<Change> {
        self.channel_members.insert(
            channel.clone(),
            users.iter().map(|u| u.id.clone()).collect(),
        );
        self.apply_users(users)
    }

    /// Move a channel into a category, and say which categories changed.
    ///
    /// Applied here and sent on, like folding: the sidebar has to follow
    /// the drag under the pointer rather than when the server answers.
    /// A sync overwrites it, which is what keeps the server's copy the
    /// one that matters.
    ///
    /// The returned categories are what to send back -- the one it left
    /// and the one it joined, or just one when it moved within a single
    /// category. Empty means nothing moved, which is the common case of
    /// dropping a channel exactly where it already was.
    pub fn move_channel(
        &mut self,
        channel: &ChannelId,
        into: &str,
        place: &Place,
    ) -> Vec<ChannelCategory> {
        // Found once, as an index, so that putting the channel in has
        // no "category not found" branch left to be unreachable.
        let Some(target) = self.categories.iter().position(|c| c.id == into) else {
            return Vec::new();
        };
        // Dropped on itself. Taking it out and putting it back would
        // land it at the end of its own category, which is a move the
        // reader did not ask for.
        if *place == Place::Before(channel.clone()) {
            return Vec::new();
        }
        let before: Vec<Vec<ChannelId>> = self
            .categories
            .iter()
            .map(|c| c.channel_ids.clone())
            .collect();

        // Out of wherever it was -- every category, because a channel in
        // two of them is a server state this client should heal rather
        // than preserve.
        for category in &mut self.categories {
            category.channel_ids.retain(|id| id != channel);
        }
        // And into the new one, at the asked-for spot.
        let target = &mut self.categories[target];
        let at = match place {
            Place::Front => 0,
            Place::Before(neighbour) => target
                .channel_ids
                .iter()
                .position(|id| id == neighbour)
                .unwrap_or(target.channel_ids.len()),
        };
        target.channel_ids.insert(at, channel.clone());

        self.categories
            .iter()
            .zip(before)
            .filter(|(now, then)| now.channel_ids != *then)
            .map(|(now, _)| now.clone())
            .collect()
    }

    /// Take in a page of posts that answers a question rather than
    /// being a stretch of a channel.
    ///
    /// The posts join the one post map like any others, so a search hit
    /// and the same message read in its channel are one record and an
    /// edit to it shows in both. **They do not join a timeline**: a
    /// search result is next to the result above it for reasons that
    /// have nothing to do with time, and filing them as history would
    /// be the client inventing adjacency the server never claimed --
    /// which is exactly the lie the segment model exists to prevent.
    ///
    /// The order comes back rather than being stored here, because
    /// which list it belongs to is the caller's question.
    fn take_found(&mut self, page: &PostList) -> Vec<PostId> {
        for post in page.posts.values() {
            self.store_post(post.clone());
        }
        page.order
            .iter()
            .filter(|id| self.posts.get(*id).is_some_and(|p| !p.is_deleted()))
            .cloned()
            .collect()
    }

    /// What a search answered, newest first as the server ordered it.
    /// What a search answered.
    ///
    /// `append` is the second and later pages. Explicit rather than
    /// inferred from the terms: the same search run again is a replace,
    /// not a doubling of its own results, and nothing but the caller
    /// knows which it asked for.
    pub fn apply_search(&mut self, terms: &str, page: &PostList, append: bool) -> Vec<Change> {
        terms.clone_into(&mut self.search_terms);
        let found = self.take_found(page);
        if append {
            // Deduped, because the server pages by offset and a message
            // posted between two requests shifts everything down one.
            for id in found {
                if !self.search_results.contains(&id) {
                    self.search_results.push(id);
                }
            }
        } else {
            self.search_results = found;
        }
        vec![Change::Found]
    }

    /// What is pinned in one channel.
    pub fn apply_pinned(&mut self, channel: &ChannelId, page: &PostList) -> Vec<Change> {
        let found = self.take_found(page);
        self.pinned.insert(channel.clone(), found);
        vec![Change::Found]
    }

    /// What this reader has saved, across every channel.
    pub fn apply_saved(&mut self, page: &PostList) -> Vec<Change> {
        self.saved = self.take_found(page);
        vec![Change::Found]
    }

    /// One answer list, as the posts themselves.
    #[must_use]
    pub fn found(&self, which: &Found) -> Vec<&Post> {
        let ids = match which {
            Found::Search => &self.search_results,
            Found::Saved => &self.saved,
            Found::Pinned(channel) => match self.pinned.get(channel) {
                Some(ids) => ids,
                None => return Vec::new(),
            },
        };
        ids.iter().filter_map(|id| self.posts.get(id)).collect()
    }

    /// Fold a category shut, or open it again.
    ///
    /// The server owns this -- `collapsed` is a field on the category, and
    /// it follows the reader to their other clients -- but the sidebar has
    /// to fold under the click rather than when the next sync answers, so
    /// it is applied here and sent on. A sync overwrites it, which is what
    /// makes the server's copy the one that matters.
    pub fn set_category_collapsed(&mut self, id: &str, collapsed: bool) -> Vec<Change> {
        let Some(category) = self.categories.iter_mut().find(|c| c.id == id) else {
            return Vec::new();
        };
        if category.collapsed == collapsed {
            return Vec::new();
        }
        category.collapsed = collapsed;
        vec![Change::Sidebar]
    }

    /// One category as the server last stated it, for sending back.
    #[must_use]
    pub fn category(&self, id: &str) -> Option<&ChannelCategory> {
        self.categories.iter().find(|c| c.id == id)
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
    pub fn channel_heading(&self, channel: &ChannelId) -> Heading {
        let Some(open) = self.channels.get(channel) else {
            return Heading {
                name: channel.to_string(),
                ..Heading::default()
            };
        };
        let name = self.channel_label(open);
        let count = match open.kind {
            // "2 members" over somebody's name says nothing anybody needed
            // to be told, and Mattermost shows no count for a DM either.
            ChannelType::Direct => None,
            // `/stats` counts only *active* members, so a group of five
            // with two deactivated people in it reported three -- under a
            // title listing all five. The participants are the names in
            // that title, which is the number the reader can check.
            ChannelType::Group => participants(&open.display_name),
            _ => self.stats.get(channel).map(|s| s.member_count),
        };
        Heading {
            name,
            members: count,
            // The channel header, which Mattermost shows beside the name.
            // Only its first line: it is markdown and people put several
            // paragraphs of links in it, and a title bar has one line.
            topic: open
                .header
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| mm_md::parse(line).to_plain())
                .unwrap_or_default(),
        }
    }

    /// What to call a person, in the format this server asked for.
    ///
    /// Falls back to the raw id for someone not in the roster, for the same
    /// reason `channel_label` does: showing something beats showing a blank
    /// where a name should be.
    #[must_use]
    pub fn user_label(&self, user: &UserId) -> String {
        self.users
            .get(user)
            .map_or_else(|| user.to_string(), |u| u.name_as(self.name_format))
    }

    /// Whether a name after an `@` is somebody, and whether it is the
    /// reader -- the roster `mm-md` needs to mark mentions.
    ///
    /// `@all`, `@channel` and `@here` are not users and are always
    /// mentions; Mattermost highlights them the same way.
    #[must_use]
    pub fn mentioned(&self, name: &str) -> Option<mm_md::Mentioned> {
        if is_special_mention(name) {
            return Some(mm_md::Mentioned::Someone);
        }
        // The reader first, by name: they are on nobody's DM list but
        // their own, and in a channel they have not spoken in they are
        // not in the roster either -- which left "@me" plain text.
        if !self.my_username.is_empty() && name == self.my_username {
            return Some(mm_md::Mentioned::Me);
        }
        let user = self.users.values().find(|u| u.username == name)?;
        Some(if user.id == self.me {
            mm_md::Mentioned::Me
        } else {
            mm_md::Mentioned::Someone
        })
    }

    /// What to put on a post's author line, and whether to mark it a bot.
    ///
    /// A webhook's chosen name wins over its account's: one bot account
    /// posts as "Alertmanager", "Deploys" and "CI", and showing the account
    /// labels every alert in the organisation with the same wrong word.
    #[must_use]
    pub fn post_author(&self, post: &Post) -> (String, bool) {
        if let Some(name) = post.override_username() {
            return (name.to_owned(), true);
        }
        let is_bot =
            post.is_from_webhook() || self.users.get(&post.user_id).is_some_and(|u| u.is_bot);
        (self.user_label(&post.user_id), is_bot)
    }

    /// When the reader last viewed a channel, as the server knows it.
    ///
    /// The window captures this the moment a channel is opened, before
    /// marking it read, because opening it moves this forward and the
    /// "new messages" line has to remember where the reader actually was.
    #[must_use]
    pub fn last_viewed(&self, channel: &ChannelId) -> Option<Timestamp> {
        self.my_member.get(channel)?.last_viewed_at
    }

    /// Whether the channel's history is fully loaded, so the view can stop
    /// asking for older pages at the top of the scrollback.
    ///
    /// An unknown channel answers `false`: nothing has been loaded, so there
    /// is certainly more to fetch.
    #[must_use]
    pub fn older_complete(&self, channel: &ChannelId) -> bool {
        self.timelines
            .get(channel)
            .is_some_and(Timeline::older_complete)
    }

    /// Forget every channel but these.
    ///
    /// A sidebar painted from a snapshot shows what was true when the
    /// snapshot was written. A channel left, archived or removed since
    /// then would otherwise sit in the list forever, because every other
    /// `apply_*` merges rather than replaces -- deliberately, since a
    /// partial page must never delete what it does not mention. This is
    /// the one place that may delete, and it is called with the whole
    /// list the server just gave us.
    pub fn retain_channels(&mut self, keep: &[ChannelId]) -> Vec<Change> {
        let keep: BTreeSet<&ChannelId> = keep.iter().collect();
        let gone: Vec<ChannelId> = self
            .my_member
            .keys()
            .chain(self.channels.keys())
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect();
        if gone.is_empty() {
            return Vec::new();
        }
        for id in &gone {
            self.channels.remove(id);
            self.my_member.remove(id);
            self.timelines.remove(id);
            self.stats.remove(id);
        }
        vec![Change::Sidebar]
    }

    /// Leave a channel here and now, without waiting for the server.
    ///
    /// Everything the client knows about it goes: it is out of the sidebar,
    /// its history is dropped, and its roster with it. Optimistic like
    /// muting and marking read -- the row disappearing when the button is
    /// pressed is the whole point, and if the request fails the next sync
    /// brings the channel back, which is the right way round. Leaving it on
    /// screen until a round trip finished would read as the click doing
    /// nothing.
    ///
    /// Also what runs when the server says somebody removed this reader,
    /// which is the same situation arriving from the other direction.
    pub fn leave_channel_locally(&mut self, channel: &ChannelId) -> Vec<Change> {
        let known = self.channels.remove(channel).is_some();
        let member = self.my_member.remove(channel).is_some();
        self.timelines.remove(channel);
        self.stats.remove(channel);
        self.channel_members.remove(channel);
        if known || member {
            vec![Change::Sidebar]
        } else {
            Vec::new()
        }
    }

    /// Record that somebody is now in a channel, or no longer is.
    ///
    /// Only the roster: whether the *reader* is still in it is a different
    /// question, answered by [`Store::leave_channel_locally`]. A channel
    /// whose roster was never fetched is left alone rather than created
    /// holding one person -- an incomplete roster that looks complete is
    /// what makes `@` offer three names in a channel of forty.
    pub fn set_channel_member(
        &mut self,
        channel: &ChannelId,
        user: &UserId,
        present: bool,
    ) -> Vec<Change> {
        let Some(roster) = self.channel_members.get_mut(channel) else {
            return Vec::new();
        };
        let changed = if present {
            roster.insert(user.clone())
        } else {
            roster.remove(user)
        };
        if changed {
            // The member count beside the channel's name is drawn from
            // `stats`, so it has to move with the roster or the two
            // disagree on screen until the channel is opened again.
            if let Some(stats) = self.stats.get_mut(channel) {
                stats.member_count = if present {
                    stats.member_count.saturating_add(1)
                } else {
                    stats.member_count.saturating_sub(1)
                };
            }
            vec![Change::Unread(channel.clone())]
        } else {
            Vec::new()
        }
    }

    /// Whether this reader has saved a post.
    ///
    /// Saved posts are preferences in the `flagged_post` category, keyed
    /// by post id -- there is no field on the post itself.
    #[must_use]
    pub fn is_saved(&self, post: &PostId) -> bool {
        self.prefs
            .get(&("flagged_post".to_owned(), post.to_string()))
            .is_some_and(|value| mm_types::is_true(value))
    }

    /// Record that a post has just been saved, or unsaved.
    ///
    /// Applied here as well as sent, like a folded category, and for a
    /// sharper reason: saving is a **preference**, and the server
    /// announces a changed preference over a WebSocket event this
    /// client does not decode. Without this the menu went on offering
    /// "Remove from saved" for a post that had just been unsaved, and
    /// the next click would have asked the server to unsave it twice.
    /// A sync overwrites it, which is what keeps the server's copy the
    /// one that matters.
    pub fn set_saved_locally(&mut self, post: &PostId, saved: bool) -> Vec<Change> {
        let key = (mm_types::keys::FLAGGED_POST.to_owned(), post.to_string());
        if saved {
            self.prefs.insert(key, "true".to_owned());
        } else {
            self.prefs.remove(&key);
            // And out of the list, so the page it was taken off
            // redraws without waiting for the server to answer.
            self.saved.retain(|held| held != post);
        }
        vec![Change::Found]
    }

    /// A channel archived -- by anybody, over the socket -- or brought
    /// back: the sidebar leaves out the archived, and what one holds is
    /// kept, since it can still be read. Nothing for a channel the store
    /// does not hold.
    fn set_channel_archived(&mut self, channel: &ChannelId, at: Timestamp) -> Vec<Change> {
        match self.channels.get_mut(channel) {
            Some(held) if held.delete_at != at => {
                held.delete_at = at;
                vec![Change::Sidebar]
            }
            _ => Vec::new(),
        }
    }

    /// Preferences changed in another client -- saving a post, mostly,
    /// which is a preference on the wire. Kept in step here the way a
    /// sync would keep it, so the menu offers the right half of save and
    /// unsave, and the saved list gains or drops the post without a
    /// round trip. The server orders that list, so a window showing it
    /// asks again; this keeps the window honest until the answer comes.
    fn on_preferences_changed(&mut self, prefs: &[mm_types::Preference]) -> Vec<Change> {
        for pref in prefs {
            self.prefs.insert(
                (pref.category.clone(), pref.name.clone()),
                pref.value.clone(),
            );
            if pref.category == mm_types::keys::FLAGGED_POST {
                let post = PostId::new(&pref.name);
                if mm_types::is_true(&pref.value) {
                    if !self.saved.contains(&post) {
                        self.saved.insert(0, post);
                    }
                } else {
                    self.saved.retain(|held| *held != post);
                }
            }
        }
        vec![Change::Found]
    }

    /// The other half: preferences removed, which is how a post is
    /// unsaved by the webapp.
    fn on_preferences_deleted(&mut self, prefs: &[mm_types::Preference]) -> Vec<Change> {
        for pref in prefs {
            self.prefs
                .remove(&(pref.category.clone(), pref.name.clone()));
            if pref.category == mm_types::keys::FLAGGED_POST {
                let post = PostId::new(&pref.name);
                self.saved.retain(|held| *held != post);
            }
        }
        vec![Change::Found]
    }

    /// Who reacted to a post with one emoji, by the names the window
    /// shows, the reader first and as "you". In the order they reacted.
    #[must_use]
    pub fn reacted_with(&self, post: &PostId, emoji: &str) -> Vec<String> {
        let Some(held) = self.reactions.get(post) else {
            return Vec::new();
        };
        let mut who: Vec<(bool, Timestamp, String)> = held
            .iter()
            .filter(|((name, _), _)| name == emoji)
            .map(|((_, user), at)| {
                let me = *user == self.me;
                let name = if me {
                    "you".to_owned()
                } else {
                    self.user_label(user)
                };
                (!me, *at, name)
            })
            .collect();
        who.sort();
        who.into_iter().map(|(_, _, name)| name).collect()
    }

    /// Whether this reader wrote a post, which decides whether they may
    /// edit or delete it.
    #[must_use]
    pub fn is_mine(&self, post: &PostId) -> bool {
        self.posts.get(post).is_some_and(|p| p.user_id == self.me)
    }

    /// Whether this reader may still edit a post: theirs, and not older
    /// than the server's edit limit allows. The server has the last
    /// word; this spares the reader an editor that cannot save.
    #[must_use]
    pub fn may_edit(&self, post: &PostId, now: Timestamp) -> bool {
        self.is_mine(post)
            && self
                .posts
                .get(post)
                .is_some_and(|p| crate::settings::editable(self.edit_limit, p.create_at, now))
    }

    /// The emoji this reader reaches for most, commonest first.
    ///
    /// Mattermost offers a few one-click reactions on every message, and
    /// which ones depends on the person: a client that hardcodes three
    /// offers two people the wrong ones. Taken from the reactions they
    /// have actually left, and padded from a default set so a new account
    /// still gets three buttons rather than none.
    #[must_use]
    pub fn favourite_emoji(&self, want: usize) -> Vec<String> {
        /// What to offer somebody who has never reacted to anything.
        const COMMON: [&str; 3] = ["+1", "eyes", "white_check_mark"];

        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for ((emoji, user), _) in self.reactions.values().flatten() {
            if *user == self.me {
                *counts.entry(emoji.as_str()).or_default() += 1;
            }
        }
        let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
        // By count, then by name: a `BTreeMap` iterates in name order, so
        // ties are already stable and the buttons do not move about.
        ranked.sort_by(|a, b| b.1.cmp(&a.1));

        let mut out: Vec<String> = Vec::with_capacity(want);
        for (emoji, _) in ranked {
            if out.len() == want {
                break;
            }
            out.push(emoji.to_owned());
        }
        for fallback in COMMON {
            if out.len() == want {
                break;
            }
            if !out.iter().any(|held| held == fallback) {
                out.push(fallback.to_owned());
            }
        }
        out
    }

    /// A link to one post, the way Mattermost writes them.
    ///
    /// `{server}/{team}/pl/{post}` — the team is the one the post's channel
    /// belongs to, and a DM has none, so those get the first team the
    /// reader is in, which is what the web app does.
    #[must_use]
    pub fn permalink(&self, server: &str, post: &PostId) -> Option<String> {
        let channel = self.posts.get(post).map(|p| p.channel_id.clone())?;
        let team = self
            .channels
            .get(&channel)
            .and_then(|c| self.teams.iter().find(|t| t.id == c.team_id))
            .or_else(|| self.teams.first())?;
        Some(format!(
            "{}/{}/pl/{post}",
            server.trim_end_matches('/'),
            team.name
        ))
    }

    /// Whether this reader has already reacted to a post with an emoji.
    ///
    /// What makes a one-click reaction a toggle rather than an
    /// add-only button: there is no separate "take it back" affordance
    /// anywhere in Mattermost, so every way of reacting has to be able
    /// to unreact.
    #[must_use]
    pub fn reacted(&self, post: &PostId, emoji: &str) -> bool {
        self.reactions.get(post).is_some_and(|held| {
            held.keys()
                .any(|(name, user)| name == emoji && *user == self.me)
        })
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

    /// Take in the reader's preferences, replacing what is held.
    pub fn apply_preferences(&mut self, prefs: &[mm_types::Preference]) -> Vec<Change> {
        for pref in prefs {
            self.prefs.insert(
                (pref.category.clone(), pref.name.clone()),
                pref.value.clone(),
            );
        }
        vec![Change::ReadState]
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
            // The newest page does not reach what is held: the reader
            // has been away for longer than a page, and the posts in
            // between were never fetched.
            //
            // **This used to do nothing**, which meant the page was
            // thrown away and the channel went on showing yesterday's
            // messages with nothing newer -- the client silently
            // missing messages, which is the one failure a chat client
            // must not have. The plan's rule applies: discard the stale
            // run and keep the fresh tail, bounding memory and staying
            // honest, rather than paging backwards indefinitely to join
            // them. `older_complete` goes back to false, so scrolling
            // up closes the gap the ordinary way.
            Merge::WouldLeaveGap if anchor == PageAnchor::Newest => {
                timeline.reset_to_page(page);
                changes.extend(page.ordered().map(|p| Change::Post(p.id.clone())));
                changes.push(Change::Unread(channel.clone()));
            }
            // Nothing to do, for two different reasons: the page was
            // already held, or it was asked for with a cursor and its
            // gap is the caller's to close -- resetting under one of
            // those would throw away the stretch being paged through.
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
        let Some(held) = self.posts.get(&incoming.id) else {
            self.posts.insert(incoming.id.clone(), incoming);
            return true;
        };
        // An out-of-order edit must not regress a newer one, and a replayed
        // `posted` must not resurrect a deleted post.
        if held.update_at > incoming.update_at {
            return false;
        }
        let merged = Self::better_informed(held, incoming);
        if *held == merged {
            return false;
        }
        self.posts.insert(merged.id.clone(), merged);
        true
    }

    /// One post from two copies of it, keeping whichever field is known.
    ///
    /// Endpoints disagree about how much of a post they send, and the same
    /// `update_at` on both copies means neither is newer -- so "the second
    /// one loses" throws information away rather than preventing a
    /// regression. That is not hypothetical: `GET /users/{id}/teams/{id}/
    /// threads` embeds each thread's root post with **`reply_count: 0`**,
    /// while the channel page carries the real count on a byte-identical
    /// `update_at`. Whichever request answered second was discarded, so
    /// whether a thread showed "2 replies" or nothing at all came down to
    /// which of two requests won a race. It showed replies on a busy server
    /// and none on a quiet one, which is the worst way for a bug to behave.
    ///
    /// `incoming` is the base because it is the same version or newer; only
    /// the fields an endpoint can legitimately omit are taken from `held`.
    fn better_informed(held: &Post, incoming: Post) -> Post {
        let mut post = incoming;
        // A deletion is the same kind of fact: the copy that carries it
        // knows something the other one does not, and no endpoint ever
        // un-deletes a post. Without this a replayed `posted` for a
        // tombstoned id puts the message back on screen.
        if held.is_deleted() && !post.is_deleted() {
            post.delete_at = held.delete_at;
        }
        // Never reported lower by an endpoint that knows it; reported as
        // zero by every endpoint that does not.
        post.reply_count = post.reply_count.max(held.reply_count);
        // Absent metadata means "not loaded", not "no files" -- the same
        // distinction `FileRef` is documented around.
        if post.metadata.is_null() && !held.metadata.is_null() {
            post.metadata = held.metadata.clone();
        }
        post
    }

    // ------------------------------------------------------------ WebSocket

    pub fn apply_ws(&mut self, event: &WsEvent) -> Vec<Change> {
        match event {
            WsEvent::Posted { post, mentions, .. } => self.on_posted(post, mentions),
            WsEvent::PostEdited(post) => {
                if self.store_post(post.clone()) {
                    vec![Change::Post(post.id.clone())]
                } else {
                    Vec::new()
                }
            }
            WsEvent::PostDeleted(post) => self.on_deleted(post),
            WsEvent::ThreadUpdated(thread) => self.on_thread_updated(thread),
            WsEvent::ThreadReadChanged {
                thread_id,
                timestamp,
                unread_replies,
                unread_mentions,
            } => self.on_thread_read(thread_id, *timestamp, *unread_replies, *unread_mentions),
            WsEvent::EmojiAdded(emoji) => self.apply_custom_emoji(std::slice::from_ref(emoji)),
            WsEvent::UserAddedToChannel {
                channel_id,
                user_id,
            } => {
                // Being added to a channel is not a roster change for this
                // client -- it is a channel it has never heard of. The
                // sidebar cannot draw it from an id, so the caller fetches
                // it; all that happens here is the roster, for the case
                // where it is somebody else.
                self.set_channel_member(channel_id, user_id, true)
            }
            WsEvent::UserRemovedFromChannel {
                channel_id,
                user_id,
            } => {
                if *user_id == self.me {
                    self.leave_channel_locally(channel_id)
                } else {
                    self.set_channel_member(channel_id, user_id, false)
                }
            }
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
            WsEvent::ChannelUpdated(channel) => {
                // The record, with the counters this store keeps current
                // kept: see `apply_channel`. The name may have moved, so
                // the sidebar is told; the heading reads the record.
                self.apply_channel(channel.clone());
                vec![Change::Sidebar]
            }
            WsEvent::ChannelDeleted {
                channel_id,
                delete_at,
            } => self.set_channel_archived(channel_id, *delete_at),
            WsEvent::ChannelRestored { channel_id } => {
                self.set_channel_archived(channel_id, Timestamp::ZERO)
            }
            WsEvent::PreferencesChanged(prefs) => self.on_preferences_changed(prefs),
            WsEvent::PreferencesDeleted(prefs) => self.on_preferences_deleted(prefs),
            WsEvent::UserUpdated(user) => {
                let id = user.id.clone();
                self.users.insert(id.clone(), user.clone());
                vec![Change::User(id)]
            }
            WsEvent::StatusChange { user_id, status } => {
                let id = UserId::new(user_id);
                let presence = mm_types::Presence::parse(status);
                if self.statuses.insert(id.clone(), presence) == Some(presence) {
                    // The server repeats a presence when a second device
                    // reports it; redrawing every row for that is waste.
                    return Vec::new();
                }
                vec![Change::User(id)]
            }
            // Typing expires on a clock, which this store deliberately
            // does not have, so it is the window's to hold. Returning no
            // changes is correct, not a stub.
            WsEvent::Typing { .. } | WsEvent::Unknown { .. } => Vec::new(),
        }
    }

    fn on_posted(&mut self, post: &Post, mentions: &[String]) -> Vec<Change> {
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
        // The copy sent to a mentioned reader names them in `mentions`,
        // and the badge is drawn from the membership's own counter: the
        // server moves that counter without saying so, and a sync is the
        // only other thing that would. Not for the reader's own message,
        // which the server never counts against them.
        if post.user_id != self.me
            && mentions.iter().any(|who| *who == *self.me.as_str())
            && let Some(member) = self.my_member.get_mut(&post.channel_id)
        {
            member.mention_count += 1;
            if !post.is_reply() {
                member.mention_count_root += 1;
            }
            changes.push(Change::Unread(post.channel_id.clone()));
        }
        changes.extend(self.bump_thread_for(post));
        changes
    }

    /// A reply landed in a thread the reader follows: move the thread.
    ///
    /// The server says the same thing a moment later as a
    /// `thread_updated`, with the counts as it holds them, and that
    /// replaces this. But the reply itself arrives first, and the
    /// Threads view that waited for the other one was a photograph of
    /// the last sync while the channel beside it moved.
    fn bump_thread_for(&mut self, post: &Post) -> Vec<Change> {
        if !post.is_reply() {
            return Vec::new();
        }
        let mine = post.user_id == self.me;
        let Some(thread) = self
            .threads
            .iter_mut()
            .find(|held| held.id.as_str() == post.root_id.as_str())
        else {
            return Vec::new();
        };
        // Not past what the server already told us: a reply replayed
        // after a reconnect, or one the `thread_updated` for has already
        // been applied, moves nothing.
        if post.create_at <= thread.last_reply_at {
            return Vec::new();
        }
        let was_unread = thread.is_unread();
        thread.reply_count += 1;
        thread.last_reply_at = post.create_at;
        if !thread.participants.iter().any(|u| u.id == post.user_id) {
            thread.participants.push(mm_types::User {
                id: post.user_id.clone(),
                ..Default::default()
            });
        }
        if !mine {
            thread.unread_replies += 1;
            if !was_unread {
                self.thread_totals.unread_threads += 1;
            }
        }
        vec![Change::Threads, Change::Sidebar]
    }

    /// The server's own account of a followed thread, replacing ours.
    fn on_thread_updated(&mut self, thread: &mm_types::UserThread) -> Vec<Change> {
        if let Some(post) = &thread.post {
            self.store_post(post.clone());
        }
        let was_unread = self
            .threads
            .iter()
            .find(|held| held.id == thread.id)
            .is_some_and(mm_types::UserThread::is_unread);
        match self.threads.iter_mut().find(|held| held.id == thread.id) {
            Some(held) => {
                // A frame without the root keeps the one we have: the
                // view cannot draw a thread without one.
                let post = thread.post.clone().or_else(|| held.post.clone());
                *held = thread.clone();
                held.post = post;
            }
            None => self.threads.push(thread.clone()),
        }
        // The totals are the account's and arrive only with the list, so
        // they are moved by what this one thread did.
        match (was_unread, thread.is_unread()) {
            (false, true) => self.thread_totals.unread_threads += 1,
            (true, false) => {
                self.thread_totals.unread_threads =
                    self.thread_totals.unread_threads.saturating_sub(1);
            }
            _ => {}
        }
        vec![Change::Threads, Change::Sidebar]
    }

    /// The reader caught up on a thread, here or elsewhere.
    fn on_thread_read(
        &mut self,
        root: &PostId,
        upto: Timestamp,
        unread_replies: i64,
        unread_mentions: i64,
    ) -> Vec<Change> {
        let Some(thread) = self.threads.iter_mut().find(|held| held.id == *root) else {
            return Vec::new();
        };
        let was_unread = thread.is_unread();
        thread.last_viewed_at = upto;
        thread.unread_replies = unread_replies;
        thread.unread_mentions = unread_mentions;
        if was_unread && !thread.is_unread() {
            self.thread_totals.unread_threads = self.thread_totals.unread_threads.saturating_sub(1);
        }
        vec![Change::Threads, Change::Sidebar]
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

    /// Whether this reader follows a thread.
    ///
    /// Being in the followed list *is* following: the server sends no
    /// flag, which is why there is nothing else to consult.
    #[must_use]
    pub fn is_following(&self, root: &PostId) -> bool {
        self.threads.iter().any(|thread| thread.id == *root)
    }

    /// Stop following a thread, locally, and say so.
    ///
    /// Only the stopping is applied here. Starting has nothing to add
    /// -- the thread object comes from the server, and inventing one
    /// would mean a row drawn from a guess until the next sync
    /// replaced it with the truth. Unfollowing is the one that needs
    /// to be immediate, because it is done to make a noisy thread go
    /// away and a row that lingers reads as a click that missed.
    pub fn unfollow(&mut self, root: &PostId) -> Vec<Change> {
        let before = self.threads.len();
        self.threads.retain(|thread| thread.id != *root);
        if self.threads.len() == before {
            return Vec::new();
        }
        vec![Change::Threads]
    }

    /// Mute a channel, or let it speak again, and say what to send.
    ///
    /// Applied here and sent on, like folding a category: the sidebar
    /// has to go quiet under the click rather than when the server
    /// answers. A sync overwrites it, which is what makes the server's
    /// copy the one that counts.
    ///
    /// There is no mute flag on the wire -- `mark_unread` is `"mention"`
    /// for a muted channel and `"all"` for one that is not -- and that
    /// is the single thing most likely to be got wrong by somebody
    /// looking for a boolean.
    pub fn set_muted(&mut self, channel: &ChannelId, muted: bool) -> Vec<Change> {
        let Some(member) = self.my_member.get_mut(channel) else {
            return Vec::new();
        };
        if member.is_muted() == muted {
            return Vec::new();
        }
        let mark = if muted { "mention" } else { "all" };
        member
            .notify_props
            .insert("mark_unread".to_owned(), mark.to_owned());
        vec![Change::Unread(channel.clone()), Change::Sidebar]
    }

    /// How loudly a channel notifies, as the wire spells it.
    ///
    /// `"default"` means the channel has never been configured and
    /// the account's own setting decides, which is what the server
    /// stores for one nobody has touched.
    #[must_use]
    pub fn channel_notify(&self, channel: &ChannelId) -> String {
        self.my_member
            .get(channel)
            .and_then(|member| member.notify_props.get("desktop"))
            .filter(|chosen| !chosen.is_empty())
            .cloned()
            .unwrap_or_else(|| "default".to_owned())
    }

    /// Record a channel's notification level.
    pub fn set_channel_notify(&mut self, channel: &ChannelId, desktop: &str) -> Vec<Change> {
        if self.channel_notify(channel) == desktop {
            return Vec::new();
        }
        let Some(member) = self.my_member.get_mut(channel) else {
            return Vec::new();
        };
        member
            .notify_props
            .insert("desktop".to_owned(), desktop.to_owned());
        // Nothing on screen changes -- this decides what happens when
        // the next message arrives -- but the menu is drawn from it.
        vec![Change::Unread(channel.clone())]
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
        match self.other_in_dm(channel) {
            Some(other) => self.user_label(&UserId::new(other)),
            // A DM with yourself, whose name is your own id twice. It is
            // where Mattermost keeps saved notes, and it labels it with
            // your name and "(you)" -- without which the row drew as an
            // empty name beside a "#", because there was no other party
            // to name it after.
            None => format!("{} (you)", self.user_label(&self.me)),
        }
    }

    /// The id of the person on the other side of a DM, or `None` when
    /// that person is the reader themselves.
    fn other_in_dm<'a>(&self, channel: &'a Channel) -> Option<&'a str> {
        channel
            .name
            .split("__")
            .find(|part| *part != self.me.as_str())
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
    /// Whether this is a one-to-one conversation with somebody who has
    /// left the server.
    ///
    /// Mattermost keeps those out of the sidebar, and it is right to:
    /// the conversation cannot be continued, and on a server with any
    /// history the list fills up with people who no longer work here.
    /// The messages are not gone -- the channel is reachable by search
    /// and by permalink, and a departed person's posts still render
    /// wherever they appear.
    ///
    /// Only when the roster actually says so. Somebody this client has
    /// never fetched is shown, because "not known" must never be drawn
    /// as "gone".
    fn is_with_departed(&self, channel: &Channel) -> bool {
        self.other_party(channel)
            .and_then(|id| self.users.get(&id))
            .is_some_and(|user| !user.delete_at.is_zero())
    }

    fn other_party(&self, channel: &Channel) -> Option<UserId> {
        if channel.kind != ChannelType::Direct {
            return None;
        }
        // In a DM with yourself the other party is you, so the row gets
        // your own face rather than falling through to a channel icon.
        Some(UserId::new(
            self.other_in_dm(channel).unwrap_or(self.me.as_str()),
        ))
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
            .filter(|c| !self.is_with_departed(c))
            .map(|c| SidebarRow {
                channel: c.id.clone(),
                display_name: self.channel_label(c),
                kind: c.kind.clone(),
                avatar_user: self.other_party(c),
                unread: self.unread(&c.id),
                muted: self.is_muted(&c.id),
                has_draft: self.draft_for(&c.id, None).is_some(),
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

    /// The last thing this reader said in a channel.
    ///
    /// What Up in an empty composer edits, which is why it skips the
    /// things that cannot be edited: somebody else's message, a
    /// join/leave notice, and a webhook's post, which is nobody's to
    /// change. With collapsed threads a reply is not in the channel at
    /// all, so Up in the channel composer never reaches one -- which
    /// matches the webapp, where the thread's own box edits the thread.
    #[must_use]
    pub fn my_last_post(&self, channel: &ChannelId) -> Option<&Post> {
        Self::last_of_mine(self.channel_posts(channel), &self.me)
    }

    /// The last thing anybody said in a channel.
    ///
    /// What Shift+Up replies to. A join/leave notice is skipped: it
    /// is the server talking, and a thread hanging off one is not a
    /// conversation anybody wants.
    #[must_use]
    pub fn last_post(&self, channel: &ChannelId) -> Option<&Post> {
        self.channel_posts(channel)
            .into_iter()
            .rev()
            .find(|post| !post.is_system())
    }

    /// The last thing this reader said in a thread.
    #[must_use]
    pub fn my_last_reply(&self, root: &PostId) -> Option<&Post> {
        Self::last_of_mine(self.thread_posts(root), &self.me)
    }

    fn last_of_mine<'a>(posts: Vec<&'a Post>, me: &UserId) -> Option<&'a Post> {
        posts
            .into_iter()
            .rev()
            .find(|p| p.user_id == *me && !p.is_system() && !p.is_from_webhook())
    }

    /// How many replies a root post has, counting what is actually held.
    ///
    /// The server's own `reply_count` is preferred when it is set, because it
    /// counts replies this client has never fetched. Falling back to a local
    /// count keeps the number right for a thread built up live over the
    /// WebSocket, where the root post was fetched before the replies existed.
    #[must_use]
    /// Take in a thread's posts without touching any channel timeline.
    ///
    /// A thread page is not a page of a channel: with collapsed threads its
    /// replies are not in the channel at all, and merging it as one would
    /// claim an adjacency the server never stated. The posts are stored, the
    /// timeline is left alone, and `thread_posts` reads them back.
    pub fn apply_thread(&mut self, page: &PostList) -> Vec<Change> {
        page.posts
            .values()
            .filter(|post| self.store_post((*post).clone()))
            .map(|post| Change::Post(post.id.clone()))
            .collect()
    }

    /// A thread: its root, then every reply held, oldest first.
    ///
    /// Ordered by `(create_at, id)` like every other timeline here, because
    /// `create_at` alone collides in bulk imports and webhook bursts.
    /// Deleted posts are left out but a deleted *root* is not: a thread
    /// whose root was removed still has readable replies, and dropping the
    /// root would orphan them.
    #[must_use]
    pub fn thread_posts(&self, root: &PostId) -> Vec<&Post> {
        let mut replies: Vec<&Post> = self
            .posts
            .values()
            .filter(|p| p.root_id == *root && !p.is_deleted())
            .collect();
        replies.sort_by(|a, b| a.create_at.cmp(&b.create_at).then_with(|| a.id.cmp(&b.id)));
        let mut out = Vec::with_capacity(replies.len() + 1);
        out.extend(self.posts.get(root));
        out.extend(replies);
        out
    }

    #[must_use]
    pub fn reply_count(&self, root: &PostId) -> i64 {
        let stated = self.posts.get(root).map_or(0, |p| p.reply_count);
        // A followed thread states its own count, and states it correctly
        // even where the root post embedded beside it says zero. For a
        // thread in this list that makes the number right without waiting
        // for the channel page to arrive.
        let followed = self
            .threads
            .iter()
            .find(|thread| thread.id == *root)
            .map_or(0, |thread| thread.reply_count);
        let held = self
            .posts
            .values()
            .filter(|p| !p.is_deleted() && p.root_id == *root)
            .count();
        stated
            .max(followed)
            .max(i64::try_from(held).unwrap_or(i64::MAX))
    }
}

#[cfg(test)]
#[path = "tests/store.rs"]
mod tests;
