//! The WebSocket protocol, as a state machine.
//!
//! No socket appears in this module. `on_frame` takes the text of a received
//! frame and returns what it means; the frames to send come back as strings
//! from the builders. `mm-http` owns the connection.
//!
//! Two things here are easy to get wrong and both are encoded in the types:
//!
//! - **There are two unrelated `seq` counters.** The one on a frame *you* send
//!   is echoed back as `seq_reply`. The one on a server *event* is a
//!   per-connection counter that exists so you can detect loss. They share a
//!   name on the wire and nothing else, so they are called `client_seq` and
//!   `event_seq` here and never `seq`.
//! - **A `hello` on a reconnect means you lost events.** The server keeps a
//!   128-event dead queue; if your `sequence_number` is still in it, it
//!   replays and sends no `hello`. A fresh `hello` with a new `connection_id`
//!   is the signal that the queue could not cover the gap and the client must
//!   resync over REST.

use crate::error::AppError;
use mm_types::{
    Channel, ChannelId, ChannelMember, Post, PostId, Preference, Reaction, Timestamp, User, UserId,
};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Close code to report when our own ping watchdog expired.
pub const CLOSE_CLIENT_PING_TIMEOUT: u16 = 4000;
/// Close code to report when we saw an event-sequence gap.
pub const CLOSE_SEQUENCE_MISMATCH: u16 = 4001;

/// The server pings every 60s and closes the connection after 100s without a
/// pong. The client does not ping; it only answers. A watchdog slightly longer
/// than the ping interval is what detects a silently dead socket — the case
/// where the TCP connection stays ESTABLISHED and no FIN ever arrives.
pub const SERVER_PING_INTERVAL_SECS: u64 = 60;
/// How long to wait for *any* traffic before declaring the socket dead.
pub const PING_WATCHDOG_SECS: u64 = SERVER_PING_INTERVAL_SECS + 5;

/// What a received frame turned out to mean.
// Same rationale as WsEvent below: one variant carries a decoded model and
// another carries only a string, so the spread is real, but an outcome is
// produced, sent once and consumed. Boxing would add a pointer chase to every
// field access to save nothing that is ever held in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum WsOutcome {
    /// The connection handshake completed.
    Hello {
        connection_id: String,
        /// The 4-part composite `version.build.configHash.isLicensed`.
        server_version: String,
        /// True when this replaced an earlier connection, which means the
        /// dead queue could not cover the gap and a REST resync is required.
        lost_events: bool,
    },
    /// A server-initiated event.
    Event(WsEvent),
    /// A reply to something we sent.
    Reply {
        client_seq: u64,
        ok: bool,
        error: Option<AppError>,
    },
    /// Events were skipped. Treated exactly like a reconnect with loss.
    SequenceGap { expected: u64, got: u64 },
    /// The frame could not be understood. Kept rather than dropped so that a
    /// protocol change shows up in logs instead of as silence.
    Undecodable { detail: String },
}

/// The events this client acts on.
///
/// `Unknown` is deliberate and load-bearing. The server defines over a hundred
/// event types and adds more; a reducer with `_ => unreachable!()` would
/// panic the application the first time a new one arrived.
// Most variants carry a Post or a ChannelMember and one carries only a string,
// so the size spread is real. Allowed rather than boxed: an event is decoded,
// sent once through the engine-to-GUI channel, and consumed by the reducer
// immediately. They are never held in bulk, so the indirection would cost a
// pointer chase on every field access to save nothing that matters.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    Posted {
        post: Post,
        channel_display_name: String,
        sender_name: String,
        team_id: String,
        /// Present only on the copy sent to a mentioned user.
        mentions: Vec<String>,
    },
    PostEdited(Post),
    PostDeleted(Post),
    /// Newer servers. Carries a channel-id-to-timestamp map.
    MultipleChannelsViewed {
        channel_times: BTreeMap<String, i64>,
    },
    /// Servers at 8.1 and older. Renamed to `multiple_channels_viewed` in 8.2,
    /// and the published API docs still list the old name.
    ChannelViewed {
        channel_id: ChannelId,
    },
    /// The authoritative push for read state.
    ChannelMemberUpdated(ChannelMember),
    Typing {
        channel_id: ChannelId,
        user_id: String,
        /// The server calls this `parent_id`, not `root_id`.
        parent_id: String,
    },
    StatusChange {
        user_id: String,
        status: String,
    },
    /// `user` here is a nested object, unlike almost every other payload.
    UserUpdated(User),
    /// Somebody reacted. `reaction` is a JSON document inside a JSON string,
    /// like `post` and `channel`.
    ReactionAdded(Reaction),
    ReactionRemoved(Reaction),
    /// A followed thread changed: a reply landed in it, usually. Nested
    /// like `post`, and the same shape the threads endpoint returns --
    /// root post, participants, counts -- so it can stand in for one
    /// entry of that list.
    ///
    /// Without it the Threads view is a photograph of the last sync: a
    /// reply to a thread the reader follows was not shown there until
    /// the next one, while the channel it was in updated at once.
    ThreadUpdated(mm_types::UserThread),
    /// The reader caught up on a thread, here or in another client.
    /// Plain fields, not nested.
    /// A channel renamed, or its header or purpose changed, by anybody.
    /// The whole record, as a JSON document inside a JSON string.
    ChannelUpdated(Channel),
    /// A channel archived, by anybody: it leaves the sidebar, and what it
    /// holds can still be read.
    ChannelDeleted {
        channel_id: ChannelId,
        delete_at: Timestamp,
    },
    /// An archived channel brought back.
    ChannelRestored {
        channel_id: ChannelId,
    },
    /// Preferences written from another client of this account. Saving
    /// a post is one: `flagged_post`, keyed by post id, `"true"`.
    PreferencesChanged(Vec<Preference>),
    /// Preferences removed from another client, which is how a post is
    /// unsaved.
    PreferencesDeleted(Vec<Preference>),
    ThreadReadChanged {
        thread_id: PostId,
        /// The moment read up to.
        timestamp: mm_types::Timestamp,
        unread_replies: i64,
        unread_mentions: i64,
    },
    /// Somebody uploaded a custom emoji. Nested like `reaction`.
    ///
    /// Worth decoding for one reason: without it a newly uploaded emoji
    /// is unknown until the next sync, so the person who has just added
    /// one sees their own shortcode as text when they use it.
    EmojiAdded(mm_types::CustomEmoji),
    /// Somebody joined a channel, or was added to one.
    ///
    /// The channel is in `broadcast`, never in `data`, and the same shape
    /// reaches everyone: the person added gets `broadcast.user_id` set to
    /// themselves as well, which is the only difference.
    UserAddedToChannel {
        channel_id: ChannelId,
        user_id: UserId,
    },
    /// Somebody left a channel, or was removed from one.
    ///
    /// **Not the mirror image of `user_added`.** The copy sent to everyone
    /// still in the channel carries `broadcast.channel_id` and
    /// `data.user_id`; the copy sent to the person who left carries
    /// `data.channel_id` and `broadcast.user_id`, with **no**
    /// `broadcast.channel_id` at all. Reading one field for each from
    /// whichever side has it is not tidiness, it is the difference between
    /// noticing you were removed and not.
    ///
    /// Measured in the lab against 12.0.1, both directions.
    UserRemovedFromChannel {
        channel_id: ChannelId,
        user_id: UserId,
    },
    /// Anything else, kept so it can be logged and counted.
    Unknown {
        event: String,
    },
}

/// The envelope that says who a frame was addressed to.
///
/// Several events put what identifies them here rather than in `data`, so a
/// decoder that reads `data` alone classifies them as `Unknown` and the
/// client simply never notices what happened.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Broadcast {
    #[serde(default)]
    pub(crate) channel_id: String,
    #[serde(default)]
    pub(crate) user_id: String,
}

#[derive(Debug, Deserialize)]
struct RawFrame {
    #[serde(default)]
    event: String,
    #[serde(default)]
    data: serde_json::Value,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    seq_reply: Option<u64>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<AppError>,
    #[serde(default)]
    broadcast: Broadcast,
}

/// Connection state and sequence bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct WsProtocol {
    next_client_seq: u64,
    last_event_seq: Option<u64>,
    connection_id: Option<String>,
    server_version: Option<String>,
}

impl WsProtocol {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The authentication frame.
    ///
    /// The server rejects `seq <= 0`, so client sequence numbers start at 1.
    /// A bad token does not produce an error reply — the server closes the
    /// socket without saying anything, so "closed immediately after auth" is
    /// the failure signal to code against.
    pub fn auth_frame(&mut self, token: &str) -> String {
        let seq = self.take_client_seq();
        format!(
            r#"{{"seq":{seq},"action":"authentication_challenge","data":{{"token":{}}}}}"#,
            json_str(token)
        )
    }

    /// Tell the server we are typing. Note `parent_id`, not `root_id`.
    pub fn typing_frame(&mut self, channel: &ChannelId, parent: &str) -> String {
        let seq = self.take_client_seq();
        format!(
            r#"{{"seq":{seq},"action":"user_typing","data":{{"channel_id":{},"parent_id":{}}}}}"#,
            json_str(channel.as_str()),
            json_str(parent)
        )
    }

    fn take_client_seq(&mut self) -> u64 {
        self.next_client_seq = self.next_client_seq.max(1);
        let seq = self.next_client_seq;
        self.next_client_seq += 1;
        seq
    }

    /// Query parameters for a reliable reconnect.
    ///
    /// Sending the last connection id and the next expected event sequence is
    /// what lets the server replay from its dead queue instead of forcing a
    /// full resync.
    #[must_use]
    pub fn reconnect_query(&self, disconnect_code: Option<u16>) -> Vec<(String, String)> {
        let mut q = Vec::new();
        if let Some(id) = &self.connection_id {
            q.push(("connection_id".to_owned(), id.clone()));
            q.push((
                "sequence_number".to_owned(),
                (self.last_event_seq.unwrap_or(0) + 1).to_string(),
            ));
        }
        if let Some(code) = disconnect_code {
            q.push(("disconnect_err_code".to_owned(), code.to_string()));
        }
        q
    }

    #[must_use]
    pub fn server_version(&self) -> Option<&str> {
        self.server_version.as_deref()
    }

    #[must_use]
    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    /// Forget the sequence state, so the next connection is treated as fresh.
    pub fn reset(&mut self) {
        self.last_event_seq = None;
        self.connection_id = None;
    }

    /// Interpret one received text frame.
    ///
    /// Returns a list because a single frame can mean two things at once: an
    /// event that also reveals a sequence gap.
    pub fn on_frame(&mut self, raw: &str) -> Vec<WsOutcome> {
        let frame: RawFrame = match serde_json::from_str(raw) {
            Ok(f) => f,
            Err(e) => {
                return vec![WsOutcome::Undecodable {
                    detail: format!("not a JSON frame: {e}"),
                }];
            }
        };

        // A reply is distinguished by carrying seq_reply/status, an event by
        // carrying a non-empty `event`. Checking for an absent field does not
        // work; both shapes have to be tried.
        if frame.event.is_empty() {
            if let Some(client_seq) = frame.seq_reply {
                return vec![WsOutcome::Reply {
                    client_seq,
                    ok: frame.status == "OK",
                    error: frame.error,
                }];
            }
            return vec![WsOutcome::Undecodable {
                detail: "frame has neither an event nor a seq_reply".to_owned(),
            }];
        }

        // `hello` is handled before gap detection, and must be: it does not
        // continue the previous connection's event sequence, it *restarts* it.
        // The server resets its per-connection counter to zero when it mints a
        // new connection, so running the gap check across a hello reports a
        // hole on every single reconnect — which is how this was found.
        if frame.event == "hello" {
            let connection_id = str_field(&frame.data, "connection_id");
            let server_version = str_field(&frame.data, "server_version");
            // A hello while we already held a *different* connection id means
            // the dead queue could not cover the gap.
            let lost_events = self
                .connection_id
                .as_ref()
                .is_some_and(|old| *old != connection_id);
            self.connection_id = Some(connection_id.clone());
            self.server_version = Some(server_version.clone());
            self.last_event_seq = frame.seq;
            return vec![WsOutcome::Hello {
                connection_id,
                server_version,
                lost_events,
            }];
        }

        let mut out = Vec::new();

        // Gap detection comes before the event is delivered, so a consumer
        // sees "resync needed" first and does not act on an event that
        // arrived after a hole.
        if let Some(seq) = frame.seq {
            if let Some(last) = self.last_event_seq
                && seq != last + 1
            {
                out.push(WsOutcome::SequenceGap {
                    expected: last + 1,
                    got: seq,
                });
            }
            self.last_event_seq = Some(seq);
        }

        match decode_event(&frame.event, &frame.data, &frame.broadcast) {
            Ok(event) => out.push(WsOutcome::Event(event)),
            Err(detail) => out.push(WsOutcome::Undecodable { detail }),
        }
        out
    }
}

/// Decode the payload of a known event.
///
/// The repeated `nested_json` calls are not an accident: on this protocol most
/// interesting payloads are a JSON document *inside* a JSON string, because
/// the server calls `.ToJSON()` on the model before putting it in the data
/// map. The exceptions are `user` on `user_updated` and `channel_times` on
/// `multiple_channels_viewed`, which are ordinary nested values.
fn decode_event(
    event: &str,
    data: &serde_json::Value,
    broadcast: &Broadcast,
) -> Result<WsEvent, String> {
    match event {
        "posted" => Ok(WsEvent::Posted {
            post: nested_json(data, "post")?,
            channel_display_name: str_field(data, "channel_display_name"),
            sender_name: str_field(data, "sender_name"),
            team_id: str_field(data, "team_id"),
            mentions: nested_json(data, "mentions").unwrap_or_default(),
        }),
        "post_edited" => Ok(WsEvent::PostEdited(nested_json(data, "post")?)),
        "reaction_added" => Ok(WsEvent::ReactionAdded(nested_json(data, "reaction")?)),
        "reaction_removed" => Ok(WsEvent::ReactionRemoved(nested_json(data, "reaction")?)),
        "thread_updated" => Ok(WsEvent::ThreadUpdated(nested_json(data, "thread")?)),
        // Sent to the whole team, so the broadcast names no channel: the
        // channel is in the data, with the moment. The broadcast is a
        // fallback for a server that says it there instead.
        "channel_updated" => Ok(WsEvent::ChannelUpdated(nested_json(data, "channel")?)),
        "channel_deleted" => Ok(WsEvent::ChannelDeleted {
            channel_id: ChannelId::new(channel_in(data, broadcast)),
            delete_at: Timestamp(int_field(data, "delete_at")),
        }),
        "channel_restored" => Ok(WsEvent::ChannelRestored {
            channel_id: ChannelId::new(channel_in(data, broadcast)),
        }),
        // Like `post`: a JSON array inside a JSON string.
        "preferences_changed" => Ok(WsEvent::PreferencesChanged(nested_json(
            data,
            "preferences",
        )?)),
        "preferences_deleted" => Ok(WsEvent::PreferencesDeleted(nested_json(
            data,
            "preferences",
        )?)),
        "thread_read_changed" => Ok(WsEvent::ThreadReadChanged {
            thread_id: PostId::new(str_field(data, "thread_id")),
            timestamp: mm_types::Timestamp(int_field(data, "timestamp")),
            unread_replies: int_field(data, "unread_replies"),
            unread_mentions: int_field(data, "unread_mentions"),
        }),
        "emoji_added" => Ok(WsEvent::EmojiAdded(nested_json(data, "emoji")?)),
        "post_deleted" => Ok(WsEvent::PostDeleted(nested_json(data, "post")?)),
        "multiple_channels_viewed" => Ok(WsEvent::MultipleChannelsViewed {
            channel_times: serde_json::from_value(
                data.get("channel_times").cloned().unwrap_or_default(),
            )
            .map_err(|e| format!("multiple_channels_viewed.channel_times: {e}"))?,
        }),
        "channel_viewed" => Ok(WsEvent::ChannelViewed {
            channel_id: ChannelId::new(str_field(data, "channel_id")),
        }),
        "channel_member_updated" => Ok(WsEvent::ChannelMemberUpdated(nested_json(
            data,
            "channelMember",
        )?)),
        // The server puts the channel on the broadcast, not in the data;
        // the data is read first in case one ever says it there.
        "typing" => Ok(WsEvent::Typing {
            channel_id: ChannelId::new(match str_field(data, "channel_id") {
                said if said.is_empty() => broadcast.channel_id.clone(),
                said => said,
            }),
            user_id: str_field(data, "user_id"),
            parent_id: str_field(data, "parent_id"),
        }),
        "status_change" => Ok(WsEvent::StatusChange {
            user_id: str_field(data, "user_id"),
            status: str_field(data, "status"),
        }),
        "user_updated" => Ok(WsEvent::UserUpdated(
            serde_json::from_value(data.get("user").cloned().unwrap_or_default())
                .map_err(|e| format!("user_updated.user: {e}"))?,
        )),
        "user_added" => Ok(WsEvent::UserAddedToChannel {
            channel_id: ChannelId::new(broadcast.channel_id.clone()),
            user_id: UserId::new(str_field(data, "user_id")),
        }),
        // Each field from whichever side of the frame carries it: see the
        // variant's documentation for why the two copies differ.
        "user_removed" => Ok(WsEvent::UserRemovedFromChannel {
            channel_id: ChannelId::new(match str_field(data, "channel_id") {
                empty if empty.is_empty() => broadcast.channel_id.clone(),
                stated => stated,
            }),
            user_id: UserId::new(match str_field(data, "user_id") {
                empty if empty.is_empty() => broadcast.user_id.clone(),
                stated => stated,
            }),
        }),
        other => Ok(WsEvent::Unknown {
            event: other.to_owned(),
        }),
    }
}

/// The channel a frame is about: said in the data when the frame went
/// to a whole team, on the broadcast when it went to a channel.
fn channel_in(data: &serde_json::Value, broadcast: &Broadcast) -> String {
    match str_field(data, "channel_id") {
        said if said.is_empty() => broadcast.channel_id.clone(),
        said => said,
    }
}

/// Read a field whose value is a JSON document inside a JSON string.
fn nested_json<T: for<'de> Deserialize<'de>>(
    data: &serde_json::Value,
    field: &str,
) -> Result<T, String> {
    let raw = data
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "field `{field}` is missing or not a string; on this protocol it carries a \
                 JSON document inside a JSON string"
            )
        })?;
    serde_json::from_str(raw).map_err(|e| format!("field `{field}` contained invalid JSON: {e}"))
}

/// A number the server sends as a number. Missing or not a number is
/// zero, the same way a missing string is empty: the frame is still a
/// frame, and the field says nothing.
fn int_field(data: &serde_json::Value, field: &str) -> i64 {
    data.get(field)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default()
}

fn str_field(data: &serde_json::Value, field: &str) -> String {
    data.get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Quote and escape a string for embedding in a frame.
///
/// Via `Value`'s `Display` rather than `serde_json::to_string`, because that
/// returns a `Result` whose error arm cannot happen for a string — and an
/// unreachable fallback is a line no test can ever cover. Going through
/// `Value::String` makes the operation infallible in the type system instead
/// of papering over an impossible case.
fn json_str(raw: &str) -> String {
    serde_json::Value::String(raw.to_owned()).to_string()
}

/// Full-jitter exponential backoff.
///
/// Jitter comes in as a number rather than from an RNG so that the schedule is
/// a pure function and can be asserted. A reconnect loop without jitter is how
/// a client hammers a server at tens of attempts per second and gets its
/// address rate-limited.
#[must_use]
pub fn next_delay_secs(attempt: u32, jitter_0_to_1: f64) -> f64 {
    const BASE: f64 = 1.0;
    const CAP: f64 = 30.0;
    // `f64::from` on a u32 rather than a cast: the exponent is bounded at 16,
    // so the shift is at most 65536 and the conversion is lossless *and*
    // infallible in the type system, which a cast is not.
    let exp = attempt.min(16);
    let ceiling = (BASE * f64::from(1_u32 << exp)).min(CAP);
    ceiling * jitter_0_to_1.clamp(0.0, 1.0)
}

#[cfg(test)]
#[path = "tests/ws.rs"]
mod tests;
