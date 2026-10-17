//! The hub: topics, rooms, presence and revocations, one per process, in `AppState`
//! (Phoenix.PubSub + Phoenix.Presence + a GenServer per room).
//!
//! GUIDE: .offrails/guides/channels.md#the-hub-kernelhubrs
//!
//! - **One actor per topic, one command at a time.** A room's state lives in a tokio task with a
//!   mailbox, so its handlers never race (a rate limit or a counter needs no lock). A panic in a
//!   room answers `unavailable`, drops the state and counts a restart; the next command rebuilds it
//!   with `Room::init`.
//! - **Serialize once.** A broadcast builds one `Frame`; every subscriber gets a clone of the same
//!   buffer (a refcount bump), except for the few events a room intercepts (`Room::handle_out`).
//! - **Presence lists people, not tabs.** Refs are `(node, connection)`: a person joins with their
//!   first ref and leaves with their last, on any node.
//! - **Wake only who it's for.** One broadcast sender per topic and one revocation sender per
//!   member key: a logout wakes that person's connections, never every socket on the node.
//! - **More than one node** goes through `kernel/cluster.rs`: broadcasts, presence, revocations
//!   and re-authorizations are published on the database bus and applied by every other node.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::FutureExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::kernel::channels::SocketConfig;
use crate::kernel::cluster::{BusMsg, Cluster, ClusterPeer, Envelope, SyncRef};
use crate::kernel::transports::Relays;

/// A JSON value: event data, presence metadata, a reply.
pub type Payload = serde_json::Value;
/// One offrails.v1 frame, serialized once; clones share the buffer.
pub type Frame = axum::extract::ws::Utf8Bytes;

/// Frames a topic buffers per subscriber. At 3/4 a connection is warned (`slow`); past it the
/// connection is dropped (`too_slow`) and resumes from its cursor when it reconnects.
pub const TOPIC_BUFFER: usize = 256;
/// Commands queued per room before senders wait.
const MAILBOX: usize = 256;
/// A room with no commands for this long stops; the next command rebuilds it.
const ACTOR_IDLE: Duration = Duration::from_secs(300);
/// Frames kept per connection for `/dev/sockets` (development only).
pub const FRAME_LOG: usize = 40;
/// Bytes kept per logged frame, cut on a char boundary, then "…".
const FRAME_LOG_TEXT: usize = 400;

/// A person on a connection, decided by the server from the session (never from a frame).
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    /// The person (a user id): presence, revocations and per-user caps are keyed by it.
    pub key: String,
    /// What others see in presence. Minimal: a display name and initials, never an email.
    pub meta: Payload,
    /// The session this connection authenticated with; re-checked while it lives.
    pub session: Option<i64>,
}

/// A refusal, sent as an `err` frame: `code` is stable (for programs), `message` is a sentence
/// (for people).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub code: String,
    pub message: String,
}

impl Rejection {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
    pub fn forbidden() -> Self {
        Self::new("forbidden", "You can't join that topic.")
    }
    pub fn unknown_topic() -> Self {
        Self::new("unknown_topic", "There is no such topic.")
    }
    pub fn not_joined() -> Self {
        Self::new("not_joined", "Join the topic first.")
    }
    pub fn bad_event() -> Self {
        Self::new("bad_event", "That event isn't accepted here.")
    }
    pub fn unavailable() -> Self {
        Self::new("unavailable", "The room is restarting; try again.")
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new("rate_limited", message)
    }
}

/// What a room answers a join with.
pub struct Joined<O> {
    /// The join's `ok.data` (a snapshot, for rooms that have one).
    pub data: Payload,
    /// Durable events after the client's cursor, oldest first: `(seq, event)`.
    pub replay: Vec<(i64, O)>,
    /// More was missed than the room replays: the client reloads its view.
    pub gap: bool,
}

/// What `Hub::join` hands the connection: the replayed events are already frames.
pub struct JoinReply {
    pub data: Payload,
    pub replay: Vec<Frame>,
    pub gap: bool,
}

/// What a room emits while handling a push. Sent after the handler returns, in order.
pub struct Outbox<O> {
    items: Vec<Emit<O>>,
}

enum Emit<O> {
    Topic {
        seq: Option<i64>,
        event: O,
        except_sender: bool,
    },
    Other {
        topic: String,
        event: O,
    },
}

impl<O> Outbox<O> {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Durable: carries `seq` (the resume cursor) and is replayed after a reconnect.
    /// INVARIANT: write to the database first, then emit (test: a_message_is_stored_before_anyone_sees_it)
    pub fn emit(&mut self, seq: i64, event: O) {
        self.items.push(Emit::Topic {
            seq: Some(seq),
            event,
            except_sender: false,
        });
    }

    /// Ephemeral (cursors, "is typing"): no `seq`, never replayed.
    pub fn emit_ephemeral(&mut self, event: O) {
        self.items.push(Emit::Topic {
            seq: None,
            event,
            except_sender: false,
        });
    }

    /// Phoenix's `broadcast_from`: everyone on the topic except the connection that pushed.
    pub fn broadcast_from(&mut self, event: O) {
        self.items.push(Emit::Topic {
            seq: None,
            event,
            except_sender: true,
        });
    }

    /// An ephemeral event on another topic (a private `user:{id}` notice).
    pub fn emit_to(&mut self, topic: impl Into<String>, event: O) {
        self.items.push(Emit::Other {
            topic: topic.into(),
            event,
        });
    }
}

/// What `Room::handle_out` decides for one subscriber.
#[derive(Debug, PartialEq)]
pub enum Delivery {
    /// Send the shared frame unchanged.
    Keep,
    /// Don't send it to this subscriber.
    Drop,
    /// Send this data instead (serialized for this subscriber only).
    Replace(Payload),
}

/// A room: the typed protocol and state behind one kind of topic (Phoenix.Channel).
///
/// `In` and `Out` are serde enums tagged `{"event": …, "data": …}`: the kernel turns every
/// `push` into `In` before the room runs (anything else is `bad_event`), and every `Out` into an
/// `event` frame.
pub trait Room: Send + Sized + 'static {
    /// The app state.
    type Ctx: Clone + Send + Sync + 'static;
    /// What clients may push.
    type In: DeserializeOwned + Send;
    /// What the room emits.
    type Out: Serialize + Send;

    /// Builds the state, at the first command and after a crash.
    fn init(ctx: &Self::Ctx, topic: &str) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// A member joined (authorization already passed): the `ok` data, and what they missed
    /// since `since`.
    fn joined(
        &mut self,
        ctx: &Self::Ctx,
        topic: &str,
        member: &Member,
        since: Option<i64>,
    ) -> impl Future<Output = Result<Joined<Self::Out>, Rejection>> + Send;

    /// One push (`handle_in/3`). The reply goes back as the push's `ok.data`.
    fn handle_in(
        &mut self,
        ctx: &Self::Ctx,
        topic: &str,
        member: &Member,
        msg: Self::In,
        out: &mut Outbox<Self::Out>,
    ) -> impl Future<Output = Result<Payload, Rejection>> + Send;

    /// Phoenix's `intercept`: the events `handle_out` sees. Every other event is serialized once
    /// and shared; an intercepted one is checked per subscriber, so keep the list short.
    fn intercepts() -> &'static [&'static str] {
        &[]
    }

    /// Phoenix's `handle_out/3`: one subscriber, one intercepted event. Runs on the connection's
    /// task (not the room's), so it is a plain function of what it's given.
    fn handle_out(_topic: &str, _member: &Member, _event: &str, _data: &Payload) -> Delivery {
        Delivery::Keep
    }
}

/// What a topic's subscribers receive.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub frame: Frame,
    pub seq: Option<i64>,
    pub event: Option<Arc<str>>,
    /// Present only for events the room intercepts.
    pub data: Option<Arc<Payload>>,
    /// `broadcast_from`: the local connection that must not get it.
    pub except: Option<u64>,
    /// Not a frame: re-check this topic's authorization now (`Hub::reauthorize`).
    pub reauthorize: bool,
}

impl Outgoing {
    fn frame(frame: Frame) -> Self {
        Self {
            frame,
            seq: None,
            event: None,
            data: None,
            except: None,
            reauthorize: false,
        }
    }
}

/// Server frames are structs, so `t` comes first and the fields keep the protocol's order (a
/// `json!` map would sort them).
#[derive(Serialize)]
struct EventFrame<'a> {
    t: &'static str,
    topic: &'a str,
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<i64>,
    data: &'a Payload,
}

#[derive(Serialize)]
struct PresenceFrame<'a> {
    t: &'static str,
    topic: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    here: Option<&'a Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    joins: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    leaves: Option<Payload>,
}

fn frame_of<T: Serialize>(value: &T) -> Frame {
    serde_json::to_string(value).unwrap_or_default().into()
}

pub fn event_frame(topic: &str, seq: Option<i64>, event: &str, data: &Payload) -> Frame {
    frame_of(&EventFrame {
        t: "event",
        topic,
        event,
        seq,
        data,
    })
}

pub fn presence_here_frame(topic: &str, here: &Payload) -> Frame {
    frame_of(&PresenceFrame {
        t: "presence",
        topic,
        here: Some(here),
        joins: None,
        leaves: None,
    })
}

pub fn presence_diff_frame(
    topic: &str,
    joins: &[(String, Payload)],
    leaves: &[(String, Payload)],
) -> Frame {
    let obj = |items: &[(String, Payload)]| {
        Payload::Object(items.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    };
    frame_of(&PresenceFrame {
        t: "presence",
        topic,
        here: None,
        joins: Some(obj(joins)),
        leaves: Some(obj(leaves)),
    })
}

/// `{"event": name, "data": {…}}` (a serde tag/content enum) → (name, data).
pub fn split_out<O: Serialize>(out: &O) -> Option<(String, Payload)> {
    let mut v = serde_json::to_value(out).ok()?;
    let event = v.get("event")?.as_str()?.to_owned();
    let data = v
        .get_mut("data")
        .map(Payload::take)
        .unwrap_or(Payload::Null);
    Some((event, data))
}

enum Command<R: Room> {
    Join {
        member: Member,
        since: Option<i64>,
        reply: oneshot::Sender<Result<JoinReply, Rejection>>,
    },
    In {
        member: Member,
        conn: u64,
        msg: R::In,
        reply: oneshot::Sender<Result<Payload, Rejection>>,
    },
}

/// A connection somewhere in the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Ref {
    node: String,
    conn: u64,
}

struct Person {
    meta: Payload,
    refs: BTreeSet<Ref>,
}

struct Topic<R: Room> {
    tx: broadcast::Sender<Outgoing>,
    subscribers: HashSet<u64>,
    mailbox: Option<mpsc::Sender<Command<R>>>,
    broadcasts: u64,
}

impl<R: Room> Topic<R> {
    fn new() -> Self {
        Self {
            tx: broadcast::channel(TOPIC_BUFFER).0,
            subscribers: HashSet::new(),
            mailbox: None,
            broadcasts: 0,
        }
    }
}

struct State<R: Room> {
    topics: HashMap<String, Topic<R>>,
    /// topic → person → refs on every node. Kept even without local subscribers, so a later
    /// local join sees people on other nodes.
    presence: HashMap<String, BTreeMap<String, Person>>,
}

/// One frame in a connection's log (`/dev/sockets`).
#[derive(Clone, Debug, Serialize)]
pub struct LoggedFrame {
    /// "in" (from the client) or "out".
    pub dir: &'static str,
    pub at: DateTime<Utc>,
    pub text: String,
}

/// One live connection on this node, for caps and `/dev/sockets`.
pub struct ConnInfo {
    pub id: u64,
    pub key: String,
    /// The member's display name, if their presence metadata has one.
    pub label: String,
    /// `websocket`, `sse` or `longpoll`.
    pub transport: &'static str,
    pub opened_at: DateTime<Utc>,
    topics: Mutex<BTreeSet<String>>,
    frames_in: AtomicU64,
    frames_out: AtomicU64,
    log: Mutex<VecDeque<LoggedFrame>>,
}

#[derive(Default)]
struct Conns {
    by_id: HashMap<u64, Arc<ConnInfo>>,
    per_key: HashMap<String, usize>,
}

/// Counts one connection against its member's cap, and lists it on `/dev/sockets`, for as long
/// as it lives.
pub struct ConnGuard<R: Room> {
    hub: Hub<R>,
    info: Arc<ConnInfo>,
}

impl<R: Room> ConnGuard<R> {
    /// The connection id (unique on this node).
    pub fn id(&self) -> u64 {
        self.info.id
    }

    pub fn info(&self) -> &Arc<ConnInfo> {
        &self.info
    }
}

impl<R: Room> Drop for ConnGuard<R> {
    fn drop(&mut self) {
        self.hub.unregister(&self.info);
    }
}

/// `/dev/sockets` and `/dev/sockets.json`.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub node: String,
    pub nodes: Vec<NodeInfo>,
    pub connections: usize,
    pub frames_in: u64,
    pub frames_out: u64,
    /// Frames per second since the previous snapshot.
    pub in_per_sec: f64,
    pub out_per_sec: f64,
    pub restarts: u64,
    pub logging: bool,
    pub topics: Vec<TopicSnapshot>,
    pub conns: Vec<ConnSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeInfo {
    pub node: String,
    /// Seconds since its last heartbeat (0 for this node).
    pub seen_secs_ago: u64,
    pub this: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TopicSnapshot {
    pub topic: String,
    pub connections: usize,
    /// People present on every node.
    pub people: Vec<String>,
    pub broadcasts: u64,
    pub room_running: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConnSnapshot {
    pub id: u64,
    pub key: String,
    pub label: String,
    pub transport: &'static str,
    pub topics: Vec<String>,
    pub age_secs: i64,
    pub frames_in: u64,
    pub frames_out: u64,
    pub log: Vec<LoggedFrame>,
}

struct Inner<R: Room> {
    node: String,
    state: Mutex<State<R>>,
    conns: Mutex<Conns>,
    revocations: Mutex<HashMap<String, broadcast::Sender<()>>>,
    next_conn: AtomicU64,
    closed: watch::Sender<bool>,
    /// Bumped when this node may have missed cluster messages: every local connection
    /// reconnects (and resumes from its cursor).
    resync: watch::Sender<u64>,
    restarts: AtomicU64,
    frames_in: AtomicU64,
    frames_out: AtomicU64,
    rates: Mutex<(Instant, u64, u64)>,
    log_frames: AtomicBool,
    cluster: Cluster,
    config: Arc<SocketConfig>,
    relays: Relays,
}

pub struct Hub<R: Room> {
    inner: Arc<Inner<R>>,
}

// WHY: by hand; `#[derive(Clone)]` would require `R: Clone`.
impl<R: Room> Clone for Hub<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R: Room> std::fmt::Debug for Hub<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hub({})", self.inner.node)
    }
}

/// A poisoned lock means another thread panicked mid-update. Hub state is memory that clients
/// re-announce, so keep serving rather than take the node down.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<R: Room> Hub<R> {
    /// A single-node hub with default limits (unit tests).
    pub fn new(node: impl Into<String>) -> Self {
        let config = SocketConfig::new(vec!["http://localhost:3000".into()]);
        Self::with_cluster(Cluster::off(node), config)
    }

    pub fn with_cluster(cluster: Cluster, config: SocketConfig) -> Self {
        let (closed, _) = watch::channel(false);
        let (resync, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                node: cluster.node().to_owned(),
                state: Mutex::new(State {
                    topics: HashMap::new(),
                    presence: HashMap::new(),
                }),
                conns: Mutex::default(),
                revocations: Mutex::default(),
                next_conn: AtomicU64::new(1),
                closed,
                resync,
                restarts: AtomicU64::new(0),
                frames_in: AtomicU64::new(0),
                frames_out: AtomicU64::new(0),
                rates: Mutex::new((Instant::now(), 0, 0)),
                log_frames: AtomicBool::new(false),
                cluster,
                config: Arc::new(config),
                relays: Relays::default(),
            }),
        }
    }

    pub fn node(&self) -> &str {
        &self.inner.node
    }

    pub fn cluster(&self) -> &Cluster {
        &self.inner.cluster
    }

    /// Limits and timings of every connection.
    pub fn config(&self) -> Arc<SocketConfig> {
        self.inner.config.clone()
    }

    /// The open SSE and long-poll relays of this node.
    pub fn relays(&self) -> &Relays {
        &self.inner.relays
    }

    /// Joins the cluster bus (if any): `serve` calls it once. Tests that need several nodes call
    /// it themselves.
    pub async fn start(&self) -> anyhow::Result<()> {
        let peer: Arc<dyn ClusterPeer> = Arc::new(self.clone());
        self.inner.cluster.start(peer).await
    }

    // ---- topics ------------------------------------------------------------------------------

    /// Starts receiving `topic`. Subscribe before computing a join's reply, so nothing broadcast
    /// in between is lost (the client drops `seq <= since`).
    pub fn subscribe(&self, topic: &str, conn: u64) -> broadcast::Receiver<Outgoing> {
        let mut state = lock(&self.inner.state);
        let t = state
            .topics
            .entry(topic.to_owned())
            .or_insert_with(Topic::new);
        t.subscribers.insert(conn);
        t.tx.subscribe()
    }

    /// Stops receiving and leaves presence. The last local subscriber out drops the topic (and
    /// its room, whose mailbox closes).
    pub fn unsubscribe(&self, topic: &str, conn: u64) {
        self.untrack(topic, conn);
        let mut state = lock(&self.inner.state);
        let empty = match state.topics.get_mut(topic) {
            Some(t) => {
                t.subscribers.remove(&conn);
                t.subscribers.is_empty()
            }
            None => false,
        };
        if empty {
            state.topics.remove(topic);
        }
    }

    /// Local connections on `topic`.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        lock(&self.inner.state)
            .topics
            .get(topic)
            .map_or(0, |t| t.subscribers.len())
    }

    fn send_local(&self, topic: &str, out: Outgoing) -> usize {
        let mut state = lock(&self.inner.state);
        match state.topics.get_mut(topic) {
            Some(t) => {
                t.broadcasts += 1;
                t.tx.send(out).unwrap_or(0)
            }
            None => 0,
        }
    }

    fn outgoing(topic: &str, seq: Option<i64>, event: &str, data: &Payload) -> Outgoing {
        let intercepted = R::intercepts().contains(&event);
        Outgoing {
            frame: event_frame(topic, seq, event, data),
            seq,
            event: Some(Arc::from(event)),
            data: intercepted.then(|| Arc::new(data.clone())),
            except: None,
            reauthorize: false,
        }
    }

    /// An event to every subscriber of `topic`, on every node. Returns how many local
    /// subscribers it reached. Durable events (`seq`) must be stored first.
    pub fn broadcast(&self, topic: &str, seq: Option<i64>, event: &str, data: &Payload) -> usize {
        let n = self.send_local(topic, Self::outgoing(topic, seq, event, data));
        self.inner.cluster.publish(BusMsg::Fanout {
            topic: topic.to_owned(),
            seq,
            event: event.to_owned(),
            data: data.clone(),
        });
        n
    }

    /// Phoenix's `broadcast_from`: an ephemeral event to everyone on `topic` except local
    /// connection `conn`, on every node.
    pub fn broadcast_from(&self, conn: u64, topic: &str, event: &str, data: &Payload) -> usize {
        let mut out = Self::outgoing(topic, None, event, data);
        out.except = Some(conn);
        let n = self.send_local(topic, out);
        self.inner.cluster.publish(BusMsg::Fanout {
            topic: topic.to_owned(),
            seq: None,
            event: event.to_owned(),
            data: data.clone(),
        });
        n
    }

    /// A typed `Out` event (see `Room::Out`).
    pub fn emit(&self, topic: &str, seq: Option<i64>, out: &R::Out) -> usize {
        match split_out(out) {
            Some((event, data)) => self.broadcast(topic, seq, &event, &data),
            None => 0,
        }
    }

    /// Every connection on `topic`, on every node, re-checks `Channels::authorize` now and
    /// leaves the topic if it no longer passes (a member removed from a room).
    pub fn reauthorize(&self, topic: &str) {
        self.reauthorize_local(topic);
        self.inner.cluster.publish(BusMsg::Reauthorize {
            topic: topic.to_owned(),
        });
    }

    fn reauthorize_local(&self, topic: &str) {
        let mut out = Outgoing::frame(Frame::from_static(""));
        out.reauthorize = true;
        self.send_local(topic, out);
    }

    // ---- presence ----------------------------------------------------------------------------

    /// `member` is present on `topic` through local connection `conn` (on every node).
    pub fn track(&self, topic: &str, conn: u64, member: &Member) {
        let r = Ref {
            node: self.inner.node.clone(),
            conn,
        };
        self.track_ref(topic, r, &member.key, &member.meta);
        self.inner.cluster.publish(BusMsg::Track {
            topic: topic.to_owned(),
            conn,
            key: member.key.clone(),
            meta: member.meta.clone(),
        });
    }

    /// Local connection `conn` is no longer present on `topic`.
    pub fn untrack(&self, topic: &str, conn: u64) {
        let r = Ref {
            node: self.inner.node.clone(),
            conn,
        };
        if self.untrack_ref(topic, &r) {
            self.inner.cluster.publish(BusMsg::Untrack {
                topic: topic.to_owned(),
                conn,
            });
        }
    }

    fn track_ref(&self, topic: &str, r: Ref, key: &str, meta: &Payload) {
        let mut state = lock(&self.inner.state);
        let people = state.presence.entry(topic.to_owned()).or_default();
        let person = people.entry(key.to_owned()).or_insert_with(|| Person {
            meta: meta.clone(),
            refs: BTreeSet::new(),
        });
        let first = person.refs.is_empty();
        person.refs.insert(r);
        if first && let Some(t) = state.topics.get(topic) {
            let frame = presence_diff_frame(topic, &[(key.to_owned(), meta.clone())], &[]);
            let _ = t.tx.send(Outgoing::frame(frame));
        }
    }

    /// Returns whether the ref was present.
    fn untrack_ref(&self, topic: &str, r: &Ref) -> bool {
        let mut state = lock(&self.inner.state);
        let Some(people) = state.presence.get_mut(topic) else {
            return false;
        };
        let Some(key) = people
            .iter()
            .find(|(_, p)| p.refs.contains(r))
            .map(|(k, _)| k.clone())
        else {
            return false;
        };
        let mut left = None;
        if let Some(person) = people.get_mut(&key) {
            person.refs.remove(r);
            if person.refs.is_empty() {
                left = people.remove(&key).map(|p| (key, p.meta));
            }
        }
        if people.is_empty() {
            state.presence.remove(topic);
        }
        if let Some(left) = left
            && let Some(t) = state.topics.get(topic)
        {
            let frame = presence_diff_frame(topic, &[], &[left]);
            let _ = t.tx.send(Outgoing::frame(frame));
        }
        true
    }

    /// Everyone present on `topic`, on every node: `{"<person>": meta}`.
    pub fn presence_list(&self, topic: &str) -> Payload {
        let state = lock(&self.inner.state);
        let mut obj = serde_json::Map::new();
        if let Some(people) = state.presence.get(topic) {
            for (key, person) in people {
                obj.insert(key.clone(), person.meta.clone());
            }
        }
        Payload::Object(obj)
    }

    pub fn is_present(&self, topic: &str, key: &str) -> bool {
        lock(&self.inner.state)
            .presence
            .get(topic)
            .is_some_and(|people| people.contains_key(key))
    }

    /// Drops every ref of a node that left or died: the others see `leaves`.
    fn drop_node(&self, node: &str) {
        for (topic, r) in self.refs_of(node) {
            self.untrack_ref(&topic, &r);
        }
    }

    /// Every `(topic, ref)` a node holds.
    fn refs_of(&self, node: &str) -> Vec<(String, Ref)> {
        let state = lock(&self.inner.state);
        let mut out = Vec::new();
        for (topic, people) in &state.presence {
            for person in people.values() {
                for r in person.refs.iter().filter(|r| r.node == node) {
                    out.push((topic.clone(), r.clone()));
                }
            }
        }
        out
    }

    // ---- rooms -------------------------------------------------------------------------------

    fn mailbox(&self, ctx: &R::Ctx, topic: &str) -> mpsc::Sender<Command<R>> {
        let mut state = lock(&self.inner.state);
        let t = state
            .topics
            .entry(topic.to_owned())
            .or_insert_with(Topic::new);
        if let Some(m) = &t.mailbox
            && !m.is_closed()
        {
            return m.clone();
        }
        let (tx, rx) = mpsc::channel(MAILBOX);
        t.mailbox = Some(tx.clone());
        tokio::spawn(actor(self.clone(), ctx.clone(), topic.to_owned(), rx));
        tx
    }

    /// The room's answer to a join (authorization is the caller's: `Channels::authorize`).
    pub async fn join(
        &self,
        ctx: &R::Ctx,
        topic: &str,
        member: &Member,
        since: Option<i64>,
    ) -> Result<JoinReply, Rejection> {
        let (reply, rx) = oneshot::channel();
        self.mailbox(ctx, topic)
            .send(Command::Join {
                member: member.clone(),
                since,
                reply,
            })
            .await
            .map_err(|_| Rejection::unavailable())?;
        rx.await.map_err(|_| Rejection::unavailable())?
    }

    /// One push from local connection `conn`. The event must deserialize into `R::In`, or it is
    /// refused before the room runs.
    pub async fn handle_in(
        &self,
        ctx: &R::Ctx,
        topic: &str,
        member: &Member,
        conn: u64,
        event: &str,
        data: Payload,
    ) -> Result<Payload, Rejection> {
        // SECURITY: only the room's declared events reach it; anything else (unknown names,
        // extra or missing fields) is refused here (test: an_undeclared_event_is_bad_event_and_invalid_data_says_why).
        let msg: R::In = serde_json::from_value(json!({ "event": event, "data": data }))
            .map_err(|_| Rejection::bad_event())?;
        let (reply, rx) = oneshot::channel();
        self.mailbox(ctx, topic)
            .send(Command::In {
                member: member.clone(),
                conn,
                msg,
                reply,
            })
            .await
            .map_err(|_| Rejection::unavailable())?;
        rx.await.map_err(|_| Rejection::unavailable())?
    }

    fn deliver(&self, topic: &str, conn: u64, out: Outbox<R::Out>) {
        for item in out.items {
            match item {
                Emit::Topic {
                    seq,
                    event,
                    except_sender,
                } => {
                    let Some((name, data)) = split_out(&event) else {
                        continue;
                    };
                    if except_sender {
                        self.broadcast_from(conn, topic, &name, &data);
                    } else {
                        self.broadcast(topic, seq, &name, &data);
                    }
                }
                Emit::Other { topic, event } => {
                    if let Some((name, data)) = split_out(&event) {
                        self.broadcast(&topic, None, &name, &data);
                    }
                }
            }
        }
    }

    // ---- connections, caps, revocations ------------------------------------------------------

    /// Counts a connection for `member` without a cap (tests use it to hold slots).
    pub fn register(&self, member: &Member, transport: &'static str) -> ConnGuard<R> {
        let mut conns = lock(&self.inner.conns);
        self.register_locked(&mut conns, member, transport)
    }

    /// Counts a connection unless `member` already holds `cap` on this node. Checked and counted
    /// under one lock, so concurrent connects can't overshoot the cap.
    pub fn try_register(
        &self,
        member: &Member,
        transport: &'static str,
        cap: usize,
    ) -> Option<ConnGuard<R>> {
        let mut conns = lock(&self.inner.conns);
        if conns.per_key.get(&member.key).copied().unwrap_or(0) >= cap {
            return None;
        }
        Some(self.register_locked(&mut conns, member, transport))
    }

    fn register_locked(
        &self,
        conns: &mut Conns,
        member: &Member,
        transport: &'static str,
    ) -> ConnGuard<R> {
        let id = self.inner.next_conn.fetch_add(1, Ordering::Relaxed);
        let info = Arc::new(ConnInfo {
            id,
            key: member.key.clone(),
            label: member
                .meta
                .get("name")
                .and_then(Payload::as_str)
                .unwrap_or(&member.key)
                .to_owned(),
            transport,
            opened_at: Utc::now(),
            topics: Mutex::default(),
            frames_in: AtomicU64::new(0),
            frames_out: AtomicU64::new(0),
            log: Mutex::default(),
        });
        conns.by_id.insert(id, info.clone());
        *conns.per_key.entry(member.key.clone()).or_insert(0) += 1;
        ConnGuard {
            hub: self.clone(),
            info,
        }
    }

    fn unregister(&self, info: &ConnInfo) {
        {
            let mut conns = lock(&self.inner.conns);
            conns.by_id.remove(&info.id);
            if let Some(n) = conns.per_key.get_mut(&info.key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    conns.per_key.remove(&info.key);
                }
            }
        }
        // Forget the member's revocation sender once nobody waits on it.
        let mut rev = lock(&self.inner.revocations);
        if rev
            .get(&info.key)
            .is_some_and(|tx| tx.receiver_count() == 0)
        {
            rev.remove(&info.key);
        }
    }

    /// Open connections of `key` on this node, all transports.
    pub fn conns_of(&self, key: &str) -> usize {
        lock(&self.inner.conns)
            .per_key
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    pub fn total_conns(&self) -> usize {
        lock(&self.inner.conns).by_id.len()
    }

    /// Revocation senders alive (tests: swept after the last connection of a member goes).
    pub fn revocation_count(&self) -> usize {
        lock(&self.inner.revocations).len()
    }

    /// A session of `key` may have ended (logout, password change, revocation), on any node:
    /// wakes only `key`'s connections, and each re-checks its own session.
    pub fn revoke(&self, key: &str) {
        self.revoke_local(key);
        self.inner.cluster.publish(BusMsg::Revoke {
            key: key.to_owned(),
        });
    }

    fn revoke_local(&self, key: &str) {
        if let Some(tx) = lock(&self.inner.revocations).get(key) {
            let _ = tx.send(());
        }
    }

    /// Resolves at the next `revoke(key)`. Subscribe before the first await of a connection.
    pub fn revoked(&self, key: &str) -> impl Future<Output = ()> + Send + use<R> {
        let mut rx = lock(&self.inner.revocations)
            .entry(key.to_owned())
            .or_insert_with(|| broadcast::channel(8).0)
            .subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => return,
                    Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        }
    }

    // ---- lifecycle ---------------------------------------------------------------------------

    /// Resolves on shutdown. `use<R>`: not tied to `&self`, so a connection task can own it.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + use<R> {
        let mut rx = self.inner.closed.subscribe();
        async move {
            let _ = rx.wait_for(|c| *c).await;
        }
    }

    pub fn is_closed(&self) -> bool {
        *self.inner.closed.borrow()
    }

    /// Shutdown: every connection says `bye restart` and closes 1012; the other nodes drop this
    /// node's presence at once instead of after the heartbeat timeout.
    pub fn close(&self) {
        self.inner.cluster.leave();
        let _ = self.inner.closed.send(true);
    }

    /// Resolves when this node may have missed cluster messages (see `Inner::resync`).
    pub fn resynced(&self) -> watch::Receiver<u64> {
        self.inner.resync.subscribe()
    }

    // ---- development: frame log and snapshot --------------------------------------------------

    /// AppState turns this on in development only: production keeps no message text.
    pub fn log_frames(&self, on: bool) {
        self.inner.log_frames.store(on, Ordering::Relaxed);
    }

    pub fn is_logging(&self) -> bool {
        self.inner.log_frames.load(Ordering::Relaxed)
    }

    /// Counts a frame, and keeps its text when logging is on.
    pub fn log_frame(&self, info: &ConnInfo, dir: &'static str, text: &str) {
        let (conn_count, total) = if dir == "in" {
            (&info.frames_in, &self.inner.frames_in)
        } else {
            (&info.frames_out, &self.inner.frames_out)
        };
        conn_count.fetch_add(1, Ordering::Relaxed);
        total.fetch_add(1, Ordering::Relaxed);
        if !self.is_logging() {
            return;
        }
        let mut log = lock(&info.log);
        if log.len() >= FRAME_LOG {
            log.pop_front();
        }
        log.push_back(LoggedFrame {
            dir,
            at: Utc::now(),
            text: clip(text),
        });
    }

    pub(crate) fn set_topics(&self, info: &ConnInfo, topics: impl Iterator<Item = String>) {
        *lock(&info.topics) = topics.collect();
    }

    pub fn snapshot(&self) -> Snapshot {
        let (frames_in, frames_out) = (
            self.inner.frames_in.load(Ordering::Relaxed),
            self.inner.frames_out.load(Ordering::Relaxed),
        );
        let (in_per_sec, out_per_sec) = {
            let mut rates = lock(&self.inner.rates);
            let secs = rates.0.elapsed().as_secs_f64().max(0.001);
            let r = (
                (frames_in.saturating_sub(rates.1)) as f64 / secs,
                (frames_out.saturating_sub(rates.2)) as f64 / secs,
            );
            *rates = (Instant::now(), frames_in, frames_out);
            r
        };
        let topics = {
            let state = lock(&self.inner.state);
            let mut names: BTreeSet<&String> = state.topics.keys().collect();
            names.extend(state.presence.keys());
            names
                .into_iter()
                .map(|name| {
                    let t = state.topics.get(name);
                    TopicSnapshot {
                        topic: name.clone(),
                        connections: t.map_or(0, |t| t.subscribers.len()),
                        people: state
                            .presence
                            .get(name)
                            .map(|p| {
                                p.values()
                                    .map(|person| {
                                        person
                                            .meta
                                            .get("name")
                                            .and_then(Payload::as_str)
                                            .unwrap_or("?")
                                            .to_owned()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        broadcasts: t.map_or(0, |t| t.broadcasts),
                        room_running: t
                            .and_then(|t| t.mailbox.as_ref())
                            .is_some_and(|m| !m.is_closed()),
                    }
                })
                .collect()
        };
        let now = Utc::now();
        let mut conns: Vec<ConnSnapshot> = lock(&self.inner.conns)
            .by_id
            .values()
            .map(|c| ConnSnapshot {
                id: c.id,
                key: c.key.clone(),
                label: c.label.clone(),
                transport: c.transport,
                topics: lock(&c.topics).iter().cloned().collect(),
                age_secs: (now - c.opened_at).num_seconds(),
                frames_in: c.frames_in.load(Ordering::Relaxed),
                frames_out: c.frames_out.load(Ordering::Relaxed),
                log: lock(&c.log).iter().rev().cloned().collect(),
            })
            .collect();
        conns.sort_by_key(|c| c.id);
        Snapshot {
            node: self.inner.node.clone(),
            nodes: self.inner.cluster.nodes(),
            connections: conns.len(),
            frames_in,
            frames_out,
            in_per_sec,
            out_per_sec,
            restarts: self.inner.restarts.load(Ordering::Relaxed),
            logging: self.is_logging(),
            topics,
            conns,
        }
    }
}

/// Keeps `FRAME_LOG_TEXT` bytes, cut on a char boundary.
fn clip(text: &str) -> String {
    if text.len() <= FRAME_LOG_TEXT {
        return text.to_owned();
    }
    let mut end = FRAME_LOG_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// What the cluster bus delivers from other nodes.
impl<R: Room> ClusterPeer for Hub<R> {
    fn apply(&self, env: Envelope) {
        let node = env.node;
        match env.msg {
            BusMsg::Fanout {
                topic,
                seq,
                event,
                data,
            } => {
                self.send_local(&topic, Self::outgoing(&topic, seq, &event, &data));
            }
            BusMsg::Track {
                topic,
                conn,
                key,
                meta,
            } => self.track_ref(&topic, Ref { node, conn }, &key, &meta),
            BusMsg::Untrack { topic, conn } => {
                self.untrack_ref(&topic, &Ref { node, conn });
            }
            BusMsg::Revoke { key } => self.revoke_local(&key),
            BusMsg::Reauthorize { topic } => self.reauthorize_local(&topic),
            BusMsg::Bye => self.drop_node(&node),
            // The bus answers these itself.
            BusMsg::Hello | BusMsg::Heartbeat | BusMsg::Sync { .. } => {}
        }
    }

    /// A complete list of a node's refs: replaces what this node knew about it.
    fn replace_node(&self, node: &str, refs: Vec<SyncRef>) {
        let wanted: HashSet<(String, u64)> =
            refs.iter().map(|r| (r.topic.clone(), r.conn)).collect();
        let stale: Vec<(String, Ref)> = self
            .refs_of(node)
            .into_iter()
            .filter(|(topic, r)| !wanted.contains(&(topic.clone(), r.conn)))
            .collect();
        for (topic, r) in stale {
            self.untrack_ref(&topic, &r);
        }
        for r in refs {
            self.track_ref(
                &r.topic,
                Ref {
                    node: node.to_owned(),
                    conn: r.conn,
                },
                &r.key,
                &r.meta,
            );
        }
    }

    fn node_gone(&self, node: &str) {
        self.drop_node(node);
    }

    fn local_refs(&self) -> Vec<SyncRef> {
        let state = lock(&self.inner.state);
        let mut out = Vec::new();
        for (topic, people) in &state.presence {
            for (key, person) in people {
                for r in person.refs.iter().filter(|r| r.node == self.inner.node) {
                    out.push(SyncRef {
                        topic: topic.clone(),
                        conn: r.conn,
                        key: key.clone(),
                        meta: person.meta.clone(),
                    });
                }
            }
        }
        out
    }

    /// Messages may have been lost (the LISTEN connection dropped, or the local bus lagged):
    /// every local connection reconnects and resumes from its cursor.
    fn lost_messages(&self) {
        self.inner.resync.send_modify(|n| *n += 1);
    }
}

/// One task per topic: owns the room state and runs one command at a time, under supervision.
async fn actor<R: Room>(
    hub: Hub<R>,
    ctx: R::Ctx,
    topic: String,
    mut inbox: mpsc::Receiver<Command<R>>,
) {
    let mut state: Option<R> = None;
    loop {
        let cmd = match tokio::time::timeout(ACTOR_IDLE, inbox.recv()).await {
            Ok(Some(cmd)) => cmd,
            Ok(None) | Err(_) => break,
        };
        let room = match state.as_mut() {
            Some(room) => room,
            None => match AssertUnwindSafe(R::init(&ctx, &topic)).catch_unwind().await {
                Ok(Ok(room)) => state.insert(room),
                Ok(Err(e)) => {
                    tracing::warn!(%topic, error = %e, "room init failed");
                    reply_unavailable(cmd);
                    continue;
                }
                Err(_) => {
                    hub.inner.restarts.fetch_add(1, Ordering::Relaxed);
                    reply_unavailable(cmd);
                    continue;
                }
            },
        };
        match cmd {
            Command::Join {
                member,
                since,
                reply,
            } => {
                let r = AssertUnwindSafe(room.joined(&ctx, &topic, &member, since))
                    .catch_unwind()
                    .await;
                match r {
                    Ok(Ok(j)) => {
                        let replay = j
                            .replay
                            .into_iter()
                            .filter_map(|(seq, out)| {
                                let (event, data) = split_out(&out)?;
                                Some(event_frame(&topic, Some(seq), &event, &data))
                            })
                            .collect();
                        let _ = reply.send(Ok(JoinReply {
                            data: j.data,
                            replay,
                            gap: j.gap,
                        }));
                    }
                    Ok(Err(rejection)) => {
                        let _ = reply.send(Err(rejection));
                    }
                    Err(_) => {
                        hub.inner.restarts.fetch_add(1, Ordering::Relaxed);
                        state = None;
                        let _ = reply.send(Err(Rejection::unavailable()));
                    }
                }
            }
            Command::In {
                member,
                conn,
                msg,
                reply,
            } => {
                let mut out = Outbox::new();
                let r = AssertUnwindSafe(room.handle_in(&ctx, &topic, &member, msg, &mut out))
                    .catch_unwind()
                    .await;
                match r {
                    Ok(Ok(payload)) => {
                        hub.deliver(&topic, conn, out);
                        let _ = reply.send(Ok(payload));
                    }
                    Ok(Err(rejection)) => {
                        let _ = reply.send(Err(rejection));
                    }
                    Err(_) => {
                        hub.inner.restarts.fetch_add(1, Ordering::Relaxed);
                        state = None;
                        let _ = reply.send(Err(Rejection::unavailable()));
                    }
                }
            }
        }
    }
}

fn reply_unavailable<R: Room>(cmd: Command<R>) {
    match cmd {
        Command::Join { reply, .. } => {
            let _ = reply.send(Err(Rejection::unavailable()));
        }
        Command::In { reply, .. } => {
            let _ = reply.send(Err(Rejection::unavailable()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Ctx;

    #[derive(Deserialize)]
    #[serde(tag = "event", content = "data", rename_all = "snake_case")]
    enum In {
        Inc,
        Boom,
        Shout,
    }

    #[derive(Serialize)]
    #[serde(tag = "event", content = "data", rename_all = "snake_case")]
    enum Out {
        Count(i64),
        Shout { by: String },
    }

    struct Counter {
        n: i64,
    }

    impl Room for Counter {
        type Ctx = Ctx;
        type In = In;
        type Out = Out;
        async fn init(_: &Ctx, _: &str) -> anyhow::Result<Self> {
            Ok(Self { n: 0 })
        }
        async fn joined(
            &mut self,
            _: &Ctx,
            _: &str,
            _: &Member,
            _: Option<i64>,
        ) -> Result<Joined<Out>, Rejection> {
            Ok(Joined {
                data: json!({ "n": self.n }),
                replay: vec![],
                gap: false,
            })
        }
        async fn handle_in(
            &mut self,
            _: &Ctx,
            _: &str,
            member: &Member,
            msg: In,
            out: &mut Outbox<Out>,
        ) -> Result<Payload, Rejection> {
            match msg {
                In::Inc => {
                    self.n += 1;
                    out.emit(self.n, Out::Count(self.n));
                    Ok(json!({ "n": self.n }))
                }
                In::Boom => panic!("room bug"),
                In::Shout => {
                    out.broadcast_from(Out::Shout {
                        by: member.key.clone(),
                    });
                    Ok(Payload::Null)
                }
            }
        }
        fn intercepts() -> &'static [&'static str] {
            &["shout"]
        }
        fn handle_out(_: &str, member: &Member, _: &str, data: &Payload) -> Delivery {
            // A person never hears their own shout, in any of their tabs.
            if data["by"] == member.key.as_str() {
                Delivery::Drop
            } else {
                Delivery::Keep
            }
        }
    }

    fn member(key: &str) -> Member {
        Member {
            key: key.into(),
            meta: json!({ "name": key, "initials": key.to_uppercase() }),
            session: None,
        }
    }

    #[tokio::test]
    async fn commands_run_one_at_a_time() {
        let hub: Hub<Counter> = Hub::new("test");
        let m = member("ann");
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..200 {
            let (hub, m) = (hub.clone(), m.clone());
            set.spawn(async move {
                hub.handle_in(&Ctx, "c:1", &m, 1, "inc", Payload::Null)
                    .await
            });
        }
        while let Some(r) = set.join_next().await {
            r.unwrap().unwrap();
        }
        let joined = hub.join(&Ctx, "c:1", &m, None).await.unwrap();
        assert_eq!(joined.data["n"], 200, "200 concurrent incs end at 200");
    }

    #[tokio::test]
    async fn a_panicking_room_answers_unavailable_and_rebuilds() {
        let hub: Hub<Counter> = Hub::new("test");
        let m = member("ann");
        hub.handle_in(&Ctx, "c:2", &m, 1, "inc", Payload::Null)
            .await
            .unwrap();
        let err = hub
            .handle_in(&Ctx, "c:2", &m, 1, "boom", Payload::Null)
            .await
            .unwrap_err();
        assert_eq!(err.code, "unavailable");
        let r = hub
            .handle_in(&Ctx, "c:2", &m, 1, "inc", Payload::Null)
            .await
            .unwrap();
        assert_eq!(r["n"], 1, "the state was rebuilt by init");
        assert_eq!(hub.snapshot().restarts, 1);
    }

    #[tokio::test]
    async fn undeclared_events_are_refused_before_the_room_runs() {
        let hub: Hub<Counter> = Hub::new("test");
        let err = hub
            .handle_in(&Ctx, "c:3", &member("ann"), 1, "nope", Payload::Null)
            .await
            .unwrap_err();
        assert_eq!(err.code, "bad_event");
    }

    #[tokio::test]
    async fn presence_lists_people_not_tabs() {
        let hub: Hub<Counter> = Hub::new("test");
        let m = member("ann");
        let mut a = hub.subscribe("c:4", 1);
        let _b = hub.subscribe("c:4", 2);
        hub.track("c:4", 1, &m);
        hub.track("c:4", 2, &m);
        assert_eq!(hub.presence_list("c:4").as_object().unwrap().len(), 1);
        assert!(a.try_recv().unwrap().frame.contains("\"joins\""));
        assert!(a.try_recv().is_err(), "one joins diff for two tabs");

        hub.unsubscribe("c:4", 2);
        assert!(a.try_recv().is_err(), "ann is still here through tab 1");
        assert!(hub.is_present("c:4", "ann"));
        hub.untrack("c:4", 1);
        assert!(a.try_recv().unwrap().frame.contains("\"leaves\""));
        assert!(!hub.is_present("c:4", "ann"));
    }

    #[tokio::test]
    async fn a_broadcast_is_serialized_once_for_every_subscriber() {
        let hub: Hub<Counter> = Hub::new("test");
        let mut rxs: Vec<_> = (0..1000).map(|i| hub.subscribe("c:5", i)).collect();
        assert_eq!(hub.broadcast("c:5", Some(1), "count", &json!(1)), 1000);
        let first = rxs[0].try_recv().unwrap().frame;
        for rx in rxs.iter_mut().skip(1) {
            let f = rx.try_recv().unwrap().frame;
            assert_eq!(f.as_str().as_ptr(), first.as_str().as_ptr(), "one buffer");
        }
    }

    #[tokio::test]
    async fn broadcast_from_marks_the_sender_and_intercepted_events_carry_their_data() {
        let hub: Hub<Counter> = Hub::new("test");
        let mut rx = hub.subscribe("c:6", 7);
        hub.handle_in(&Ctx, "c:6", &member("ann"), 7, "shout", Payload::Null)
            .await
            .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.except, Some(7));
        assert_eq!(out.data.as_deref(), Some(&json!({ "by": "ann" })));
        assert_eq!(
            Counter::handle_out("c:6", &member("ann"), "shout", &json!({ "by": "ann" })),
            Delivery::Drop
        );
        assert_eq!(
            Counter::handle_out("c:6", &member("bob"), "shout", &json!({ "by": "ann" })),
            Delivery::Keep
        );
        // Not intercepted: no per-subscriber copy of the data.
        hub.broadcast("c:6", Some(9), "count", &json!(9));
        assert!(rx.try_recv().unwrap().data.is_none());
    }

    #[tokio::test]
    async fn revocation_wakes_only_that_member_and_its_sender_is_swept() {
        let hub: Hub<Counter> = Hub::new("test");
        let guard = hub.register(&member("1"), "test");
        let one = hub.revoked("1");
        let two = hub.revoked("2");
        hub.revoke("1");
        tokio::time::timeout(Duration::from_secs(1), one)
            .await
            .expect("member 1 is woken");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), two)
                .await
                .is_err(),
            "member 2 must not wake for member 1"
        );
        assert_eq!(hub.revocation_count(), 2);
        drop(guard);
        assert_eq!(hub.revocation_count(), 1, "member 1's sender is swept");
    }

    #[tokio::test]
    async fn idle_topics_are_forgotten() {
        let hub: Hub<Counter> = Hub::new("test");
        let rx = hub.subscribe("c:7", 9);
        hub.track("c:7", 9, &member("ann"));
        assert_eq!(hub.snapshot().topics.len(), 1);
        drop(rx);
        hub.unsubscribe("c:7", 9);
        assert!(hub.snapshot().topics.is_empty());
    }

    #[tokio::test]
    async fn the_cap_is_checked_and_counted_under_one_lock() {
        let hub: Hub<Counter> = Hub::new("test");
        let m = member("ann");
        let a = hub.try_register(&m, "test", 2).unwrap();
        let _b = hub.try_register(&m, "test", 2).unwrap();
        assert!(hub.try_register(&m, "test", 2).is_none());
        drop(a);
        assert!(hub.try_register(&m, "test", 2).is_some());
    }

    #[tokio::test]
    async fn the_frame_log_keeps_the_newest_clipped_frames_in_development_only() {
        let hub: Hub<Counter> = Hub::new("test");
        let guard = hub.register(&member("ann"), "test");
        hub.log_frame(guard.info(), "in", "{\"t\":\"ping\"}");
        assert!(lock(&guard.info().log).is_empty(), "logging is off");
        assert_eq!(guard.info().frames_in.load(Ordering::Relaxed), 1);

        hub.log_frames(true);
        for i in 0..=FRAME_LOG {
            hub.log_frame(guard.info(), "out", &format!("frame {i}"));
        }
        let log = lock(&guard.info().log).clone();
        assert_eq!(log.len(), FRAME_LOG);
        assert_eq!(log[0].text, "frame 1", "the oldest is dropped");
        hub.log_frame(guard.info(), "in", &"é".repeat(1000));
        let last = lock(&guard.info().log).back().unwrap().text.clone();
        assert!(last.len() <= FRAME_LOG_TEXT + "…".len() && last.ends_with('…'));
    }

    #[test]
    fn frames_start_with_their_type() {
        let ev = event_frame("room:a", Some(3), "message", &json!({ "b": 1, "a": 2 }));
        assert_eq!(
            ev.as_str(),
            r#"{"t":"event","topic":"room:a","event":"message","seq":3,"data":{"a":2,"b":1}}"#
        );
        let typing = event_frame("room:a", None, "typing", &json!({}));
        assert!(!typing.contains("seq"), "ephemeral events carry no cursor");
        let here = presence_here_frame("room:a", &json!({}));
        assert_eq!(
            here.as_str(),
            r#"{"t":"presence","topic":"room:a","here":{}}"#
        );
        let diff = presence_diff_frame("room:a", &[("1".into(), json!({}))], &[]);
        assert_eq!(
            diff.as_str(),
            r#"{"t":"presence","topic":"room:a","joins":{"1":{}},"leaves":{}}"#
        );
    }

    #[tokio::test]
    async fn closed_resolves_after_close() {
        let hub: Hub<Counter> = Hub::new("test");
        let waiting = hub.closed();
        hub.close();
        waiting.await;
        hub.closed().await;
        assert!(hub.is_closed());
    }
}
