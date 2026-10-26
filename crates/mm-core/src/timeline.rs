//! A channel's ordered posts.
//!
//! Ordering is by [`PostKey`], not by `create_at` alone. `create_at` is
//! milliseconds and collisions genuinely happen — bulk imports and webhook
//! bursts produce several posts in the same millisecond — so the id is the
//! tie-breaker. Without it the order is not total, the merge is
//! non-deterministic, and a golden suite cannot work.
//!
//! **On gaps.** `?since=` can return a page that does not overlap the retained
//! tail at all, and concatenating those produces a timeline that silently lies
//! about adjacency: the user scrolls straight past messages they never saw,
//! with nothing on screen to say so. So this type refuses to merge a page
//! whose adjacency cannot be established, and reports [`Merge::WouldLeaveGap`]
//! instead.
//!
//! Adjacency comes from **how the page was requested**, which is why
//! [`merge_page`](Timeline::merge_page) takes a [`PageAnchor`]. It cannot be
//! inferred from the page: `prev_post_id` being empty is a claim about where
//! the channel starts, not about whether the page touches what is held. An
//! earlier version of this file tried to read the cursors that way and got it
//! wrong, which a test caught.
//!
//! The full model in the plan keeps several disjoint segments at once. This
//! holds a single contiguous run, which is sufficient while every fetch is
//! anchored to it; segments become necessary when `since`-based backfill is
//! wired up.

use mm_types::{Post, PostId, PostList, Timestamp};

/// A total order over posts within a channel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PostKey {
    pub created: Timestamp,
    pub id: PostId,
}

impl PostKey {
    #[must_use]
    pub fn of(post: &Post) -> Self {
        Self {
            created: post.create_at,
            id: post.id.clone(),
        }
    }
}

/// How a page relates to what the timeline already holds.
///
/// This is the caller's knowledge, not the page's. A page fetched with
/// `before=<oldest held post>` is adjacent by construction even though it
/// shares no post with the held run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageAnchor {
    /// The newest page of the channel, fetched with no cursor.
    Newest,
    /// Fetched with `before=` the oldest post currently held.
    OlderThanHeld,
    /// Fetched with `after=` the newest post currently held.
    NewerThanHeld,
    /// Fetched with `since=`, or from any source whose adjacency is unknown.
    /// Merges only if it overlaps what is held.
    Unanchored,
}

/// What merging a page did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// Posts were added.
    Added(usize),
    /// Everything in the page was already held.
    NoChange,
    /// Merging would have invented an adjacency that does not exist. The
    /// caller must close the gap — or keep the fresh tail and drop the stale
    /// run — rather than joining the two.
    WouldLeaveGap,
}

/// One channel's posts, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    /// Ascending by [`PostKey`].
    keys: Vec<PostKey>,
    /// True once a page has proven there is nothing older.
    older_complete: bool,
}

impl Timeline {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Oldest first.
    pub fn keys(&self) -> impl Iterator<Item = &PostKey> {
        self.keys.iter()
    }

    #[must_use]
    pub fn newest(&self) -> Option<&PostKey> {
        self.keys.last()
    }

    #[must_use]
    pub fn oldest(&self) -> Option<&PostKey> {
        self.keys.first()
    }

    #[must_use]
    pub fn older_complete(&self) -> bool {
        self.older_complete
    }

    pub fn set_older_complete(&mut self, complete: bool) {
        self.older_complete = complete;
    }

    #[must_use]
    pub fn contains(&self, key: &PostKey) -> bool {
        self.keys.binary_search(key).is_ok()
    }

    /// Insert one post, keeping the order. Idempotent.
    ///
    /// This is the path a `posted` event takes, and it must tolerate the same
    /// event arriving twice — which happens whenever the server replays its
    /// dead queue after a reconnect.
    pub fn insert(&mut self, post: &Post) -> bool {
        let key = PostKey::of(post);
        match self.keys.binary_search(&key) {
            Ok(_) => false,
            Err(at) => {
                self.keys.insert(at, key);
                true
            }
        }
    }

    pub fn remove(&mut self, post: &Post) -> bool {
        let key = PostKey::of(post);
        match self.keys.binary_search(&key) {
            Ok(at) => {
                self.keys.remove(at);
                true
            }
            Err(_) => false,
        }
    }

    /// Replace everything held with this page.
    ///
    /// For when a gap is too large to close: the plan's rule is to discard the
    /// stale run and keep the fresh tail, which bounds memory and is honest,
    /// rather than paging backwards indefinitely to join them.
    pub fn reset_to_page(&mut self, page: &PostList) -> Merge {
        self.keys.clear();
        self.older_complete = false;
        self.merge_page(page, PageAnchor::Newest)
    }

    /// Merge a page of posts.
    pub fn merge_page(&mut self, page: &PostList, anchor: PageAnchor) -> Merge {
        let mut incoming: Vec<PostKey> = page
            .ordered()
            // `?since=` has no DeleteAt filter, so deleted posts do arrive on
            // a page and must not enter the ordering.
            .filter(|p| !p.is_deleted())
            .map(PostKey::of)
            .collect();
        if incoming.is_empty() {
            return Merge::NoChange;
        }
        incoming.sort();
        incoming.dedup();

        let contiguous = self.keys.is_empty()
            || match anchor {
                // Adjacent by construction: the cursor was taken from what is
                // held.
                PageAnchor::OlderThanHeld | PageAnchor::NewerThanHeld => true,
                // Overlap is the only evidence of adjacency available.
                PageAnchor::Newest | PageAnchor::Unanchored => {
                    incoming.iter().any(|k| self.contains(k))
                }
            };
        if !contiguous {
            return Merge::WouldLeaveGap;
        }

        let before = self.keys.len();
        for key in incoming {
            if let Err(at) = self.keys.binary_search(&key) {
                self.keys.insert(at, key);
            }
        }
        let added = self.keys.len() - before;
        if added == 0 {
            Merge::NoChange
        } else {
            Merge::Added(added)
        }
    }
}

#[cfg(test)]
#[path = "tests/timeline.rs"]
mod tests;
