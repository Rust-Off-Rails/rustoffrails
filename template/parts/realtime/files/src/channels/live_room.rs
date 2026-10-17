//! `room:<slug>`: a live room. Messages (durable, resumable), "is typing" (ephemeral, to everyone
//! but the typist) and waves (a private notice on the other person's `user:<id>` topic).
//!
//! GUIDE: .offrails/guides/channels.md#the-app-side
//!
//! APP: this is the example channel; copy it for your own topics (a board, a document, a match).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::kernel::hub::{Delivery, Joined, Member, Outbox, Payload, Rejection, Room};
use crate::models::room_message::{self, MessageJson, MessageParams};
use crate::models::{Errors, room};
use crate::state::AppState;

/// Messages a person may post per window, per room. APP: your pace.
pub const POSTS_PER_WINDOW: usize = 10;
pub const POST_WINDOW: Duration = Duration::from_secs(10);
/// Waves a person may send per minute.
const WAVES_PER_MINUTE: usize = 5;
/// Missed more than this many messages: `gap`, and the client reloads the room.
pub const REPLAY_LIMIT: u64 = 200;

/// What a client may push. Anything else (another event name, another field, a missing field)
/// is `bad_event` before this room runs.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum LiveIn {
    /// `{"event":"post","data":{"body":"…"}}`
    Post(MessageParams),
    /// `{"event":"typing","data":{}}`
    Typing(TypingParams),
    /// `{"event":"wave","data":{"to":7}}`
    Wave(WaveParams),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypingParams {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveParams {
    /// The person waved at: a target, never the sender (that comes from the session).
    pub to: i64,
}

/// What the room emits.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum LiveOut {
    Message(MessageJson),
    Typing { key: String, name: String },
    Waved { from: String, room: String },
}

/// May `member` be in room `slug`? A room that doesn't exist and one that's closed to them give
/// the same answer, so topic names reveal nothing.
pub async fn authorize(s: &AppState, member: &Member, slug: &str) -> Result<(), Rejection> {
    let person_id: i64 = member.key.parse().map_err(|_| Rejection::forbidden())?;
    match room::find_for(&s.db, person_id, slug).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(Rejection::forbidden()),
        // Fails closed.
        Err(_) => Err(Rejection::unavailable()),
    }
}

/// A sliding window of hits per person.
#[derive(Default)]
struct Pace(HashMap<String, VecDeque<Instant>>);

impl Pace {
    /// Counts a hit for `key`; `false` when it is over `max` within `window`.
    fn allow(&mut self, key: &str, max: usize, window: Duration) -> bool {
        let now = Instant::now();
        let hits = self.0.entry(key.to_owned()).or_default();
        while hits
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            hits.pop_front();
        }
        if hits.len() >= max {
            return false;
        }
        hits.push_back(now);
        true
    }
}

pub struct LiveRoom {
    room_id: i64,
    slug: String,
    posts: Pace,
    waves: Pace,
    typing: Pace,
}

fn invalid(errors: Errors) -> Rejection {
    Rejection::invalid(errors.full_messages().join(", "))
}

fn person_of(member: &Member) -> Result<(i64, String), Rejection> {
    let id = member.key.parse().map_err(|_| Rejection::forbidden())?;
    let name = member
        .meta
        .get("name")
        .and_then(Payload::as_str)
        .unwrap_or("Someone")
        .to_owned();
    Ok((id, name))
}

impl LiveRoom {
    pub async fn init(ctx: &AppState, topic: &str) -> anyhow::Result<Self> {
        let slug = topic.strip_prefix("room:").unwrap_or(topic);
        let room = room::find_by_slug(&ctx.db, slug)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("no room {slug}"))?;
        Ok(Self {
            room_id: room.id,
            slug: room.slug,
            posts: Pace::default(),
            waves: Pace::default(),
            typing: Pace::default(),
        })
    }

    /// The `ok` data is the room's current cursor; with `since`, the messages after it.
    pub async fn joined(
        &mut self,
        ctx: &AppState,
        since: Option<i64>,
    ) -> Result<Joined<LiveOut>, Rejection> {
        let unavailable = |_| Rejection::unavailable();
        let high = room_message::high_water(&ctx.db, self.room_id)
            .await
            .map_err(unavailable)?;
        let data = json!({ "seq": high });
        let Some(since) = since else {
            // A fresh page rendered the newest messages itself.
            return Ok(Joined {
                data,
                replay: Vec::new(),
                gap: false,
            });
        };
        // WHY: replays read the database, not this node's memory: on several nodes, messages
        // posted elsewhere are only there. One indexed query per join.
        let rows = room_message::after(&ctx.db, self.room_id, since, REPLAY_LIMIT + 1)
            .await
            .map_err(unavailable)?;
        if rows.len() as u64 > REPLAY_LIMIT || since > high {
            return Ok(Joined {
                data,
                replay: Vec::new(),
                gap: true,
            });
        }
        Ok(Joined {
            data,
            replay: rows
                .iter()
                .map(|m| (m.id, LiveOut::Message(MessageJson::from(m))))
                .collect(),
            gap: false,
        })
    }

    pub async fn handle_in(
        &mut self,
        ctx: &AppState,
        topic: &str,
        member: &Member,
        msg: LiveIn,
        out: &mut Outbox<LiveOut>,
    ) -> Result<Payload, Rejection> {
        let (person_id, name) = person_of(member)?;
        match msg {
            LiveIn::Post(params) => {
                // WHY: per person, in the room's actor, so it holds across that person's tabs.
                if !self.posts.allow(&member.key, POSTS_PER_WINDOW, POST_WINDOW) {
                    return Err(Rejection::rate_limited(
                        "Slow down: at most 10 messages in 10 seconds.",
                    ));
                }
                let body = params.validate().map_err(invalid)?;
                // INVARIANT: stored first, then emitted: a message everyone saw exists (test: a_message_is_stored_before_anyone_sees_it)
                let row =
                    room_message::create(&ctx.db, self.room_id, person_id, &name, body, Utc::now())
                        .await
                        .map_err(|_| Rejection::unavailable())?;
                out.emit(row.id, LiveOut::Message(MessageJson::from(&row)));
                Ok(json!({ "id": row.id }))
            }
            LiveIn::Typing(_) => {
                // At most one "typing" per person per second reaches the others.
                if self
                    .typing
                    .allow(&member.key, 1, Duration::from_millis(900))
                {
                    out.broadcast_from(LiveOut::Typing {
                        key: member.key.clone(),
                        name,
                    });
                }
                Ok(Payload::Null)
            }
            LiveIn::Wave(WaveParams { to }) => {
                // A wave only reaches someone who is in this room right now, and never yourself:
                // the room can't be used to ping arbitrary ids.
                if to == person_id || !ctx.hub.is_present(topic, &to.to_string()) {
                    return Err(Rejection::invalid("They're not in this room."));
                }
                if !self
                    .waves
                    .allow(&member.key, WAVES_PER_MINUTE, Duration::from_secs(60))
                {
                    return Err(Rejection::rate_limited("That's a lot of waving."));
                }
                out.emit_to(
                    format!("user:{to}"),
                    LiveOut::Waved {
                        from: name,
                        room: self.slug.clone(),
                    },
                );
                Ok(Payload::Null)
            }
        }
    }

    pub fn intercepts() -> &'static [&'static str] {
        &["typing"]
    }

    /// `broadcast_from` already skips the tab that typed; this skips the typist's other tabs.
    pub fn handle_out(member: &Member, event: &str, data: &Payload) -> Delivery {
        if event == "typing" && data.get("key").and_then(Payload::as_str) == Some(&member.key) {
            Delivery::Drop
        } else {
            Delivery::Keep
        }
    }
}

/// The topics of the app and the room behind each (`Channels::Room`).
pub enum AppRoom {
    Live(Box<LiveRoom>),
    /// `user:<id>`: private notices; nothing to push.
    Private,
}

impl Room for AppRoom {
    type Ctx = AppState;
    type In = LiveIn;
    type Out = LiveOut;

    async fn init(ctx: &AppState, topic: &str) -> anyhow::Result<Self> {
        if topic.starts_with("room:") {
            Ok(Self::Live(Box::new(LiveRoom::init(ctx, topic).await?)))
        } else {
            Ok(Self::Private)
        }
    }

    async fn joined(
        &mut self,
        ctx: &AppState,
        _topic: &str,
        _member: &Member,
        since: Option<i64>,
    ) -> Result<Joined<LiveOut>, Rejection> {
        match self {
            Self::Live(room) => room.joined(ctx, since).await,
            Self::Private => Ok(Joined {
                data: json!({}),
                replay: Vec::new(),
                gap: false,
            }),
        }
    }

    async fn handle_in(
        &mut self,
        ctx: &AppState,
        topic: &str,
        member: &Member,
        msg: LiveIn,
        out: &mut Outbox<LiveOut>,
    ) -> Result<Payload, Rejection> {
        match self {
            Self::Live(room) => room.handle_in(ctx, topic, member, msg, out).await,
            Self::Private => Err(Rejection::bad_event()),
        }
    }

    fn intercepts() -> &'static [&'static str] {
        LiveRoom::intercepts()
    }

    fn handle_out(topic: &str, member: &Member, event: &str, data: &Payload) -> Delivery {
        if topic.starts_with("room:") {
            LiveRoom::handle_out(member, event, data)
        } else {
            Delivery::Keep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pace_allows_max_per_window() {
        let mut pace = Pace::default();
        for _ in 0..3 {
            assert!(pace.allow("1", 3, Duration::from_secs(60)));
        }
        assert!(!pace.allow("1", 3, Duration::from_secs(60)));
        assert!(pace.allow("2", 3, Duration::from_secs(60)), "per person");
    }

    #[test]
    fn a_typists_other_tabs_never_hear_themselves() {
        let ann = Member {
            key: "1".into(),
            meta: json!({ "name": "Ann" }),
            session: Some(1),
        };
        let bob = Member {
            key: "2".into(),
            ..ann.clone()
        };
        let typing = json!({ "key": "1", "name": "Ann" });
        assert_eq!(
            LiveRoom::handle_out(&ann, "typing", &typing),
            Delivery::Drop
        );
        assert_eq!(
            LiveRoom::handle_out(&bob, "typing", &typing),
            Delivery::Keep
        );
    }

    #[test]
    fn pushes_are_typed_and_refuse_extra_fields() {
        let parse = |v: Payload| serde_json::from_value::<LiveIn>(v);
        assert!(parse(json!({ "event": "post", "data": { "body": "hi" } })).is_ok());
        assert!(parse(json!({ "event": "typing", "data": {} })).is_ok());
        assert!(parse(json!({ "event": "wave", "data": { "to": 2 } })).is_ok());
        assert!(
            parse(json!({ "event": "post", "data": { "body": "hi", "author_id": 2 } })).is_err()
        );
        assert!(parse(json!({ "event": "delete_everything", "data": {} })).is_err());
        assert!(parse(json!({ "event": "post", "data": {} })).is_err());
    }
}
