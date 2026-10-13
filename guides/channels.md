# Channels, Presence and the Hub (`kernel/hub.rs` + the Off Rails Wire protocol)

> **TL;DR**
> - Signals ([realtime](realtime.md)) stay the default; use channels only when the client talks back (chat, games) or you need presence.
> - One connection per tab speaking `offrails.v1` (negotiated subprotocol, JSON frames with `t`), with WebSocket → SSE+POST → long-poll fallback; resume with durable `seq` cursors.
> - Each room is an actor in `kernel/hub.rs`; typed `In`/`Out` enums; write to the DB first, then emit.
> - Security: Origin required on the handshake, session checked at upgrade and re-validated, every join and push authorized, per-user caps, revocation closes the socket (4401).
> - Multi-node is opt-in (`kernel/cluster.rs`, Postgres `NOTIFY`, rendezvous ownership).
>
> Read the rest when building chat, presence or collaborative features, or touching the wire protocol.

[realtime.md](realtime.md) covers **signals**: "book 42's reviews changed, re-fetch". That's enough for live lists, counters and notifications, and it stays the default. Channels are for what signals can't do: **the client talks back** (chat, typing, games, collaborative tools) or the page needs **who's here** (presence). The design is inspired by Phoenix Channels (one multiplexed connection per tab, topics joined with authorization, a state-owning process per room, presence that's correct across nodes), but the wire protocol is our own: **offrails.v1**, typed end to end, resumable, with transport fallback.

> Verified on 2026-10-12 in the reference app (htmx, "who's reading" avatars and a per-book discussion): axum 0.8.9 `ws`, tokio 1.53, sea-orm 2.0.4 with sqlx 0.9's `PgListener`, tokio-tungstenite 0.29 as the test client. 223 tests on SQLite and Postgres 18 (39 for realtime: protocol conformance, security, all three transports, a 3-node cluster with a node killed mid-conversation), two Playwright specs (a server restart with changes made while it was down; WebSocket → SSE → long-polling fallback), and a manual two-context Chrome session.

| Rails / Phoenix | Off Rails |
|-----------------|-----------|
| Action Cable channel / Phoenix.Channel | `src/channels/<topic>_room.rs`: a typed `Room` impl, plus an arm in `AppRoom` and `authorize` |
| Phoenix.Socket (one socket, many topics) | `GET /socket` (WebSocket, subprotocol `offrails.v1`), `kernel/channels.rs`; falls back to SSE + POST, then long-polling |
| GenServer per room, `handle_in/3` | `kernel/hub.rs` room actor: one tokio task per topic, one command at a time |
| Phoenix.PubSub | the hub's per-topic `broadcast` channel (in-process) + `NOTIFY` between nodes |
| Phoenix.Presence | `Hub::track` / `untrack`; `presence` frames (`here`, then `joins`/`leaves`) |
| Registry / Horde / libcluster | `kernel/cluster.rs`: `cluster_nodes` heartbeats + rendezvous hashing over Postgres |
| phoenix.js | `static/channels.js` (htmx) / `web/src/lib/socket.ts` (TS modes, typed) |
| LiveDashboard sockets page | `/dev/sockets`, with a live decoded frame log |

## When to use which

| The spec says… | Use |
|----------------|-----|
| "X updates without refreshing" (lists, counts, statuses) | **SSE signal** ([realtime.md](realtime.md)). No client→server messages, cheapest, works through every proxy |
| "see who's online / reading / typing" | **Channel** with presence |
| "chat", "comments appear as people type", "multiplayer", "live cursor" | **Channel** with a `Room` |
| "notify me" (personal notices) | SSE signal on `user:{id}`, or a channel topic `user:{id}` if the page already has a socket |

Never both for the same data on one page. A page may use SSE for one region and a channel for another (the reference book page does: reviews over SSE, discussion over a channel).

## How it fits together

```
browser tab ──WebSocket /socket (offrails.v1)──▶ kernel/channels.rs   Session: auth, limits, protocol
          └──or SSE /socket/sse + POST ──────────▶   (same Session, other pipes)
          └──or long-poll /socket/poll ─────────▶
                                        │ join / push
                                        ▼
                                  kernel/hub.rs  Hub<AppRoom>   (one per process, in AppState)
                                  ├─ topic "book:7": broadcast::Sender<Frame>  → every local connection on it
                                  ├─ presence {person → refs (node, conn)}
                                  └─ room actor (tokio task) owns AppRoom::Book(BookRoom) state
                                        │ durable writes first
                                        ▼
                                  database (book_messages)  ← init() rebuilds state; replay reads it
                    (cluster only)  kernel/cluster.rs: cluster_nodes heartbeat + LISTEN/NOTIFY "offrails_cluster"
```

**Memory first, database as truth.** A room's state is memory, so a join, a replay or a rate-limit check costs microseconds. Anything that must survive (messages) is written to the database **before** it's broadcast, so `Room::init` can rebuild the state after a crash, a restart, or a move to another node. Presence is never stored: clients re-announce it by rejoining.

## The wire protocol (offrails.v1)

The client opens `new WebSocket("/socket", "offrails.v1")`: the subprotocol is required and negotiated. Every frame, both directions, is a **JSON object with a `t` (type) field**. Unknown fields are ignored (forward compatibility); an unknown `t`, non-JSON, or an array is a protocol error.

**Client → server**

| `t` | Fields | Meaning |
|-----|--------|---------|
| `join` | `ref`, `topic`, `since`? | Join a topic. `since` = the last `seq` this client saw on it: the server replays every durable event after it |
| `leave` | `ref`, `topic` | Leave (presence updates for everyone else) |
| `push` | `ref`, `topic`, `event`, `data` | Send an event to the topic's room. `event` + `data` must match the room's typed `In` enum |
| `ping` | `ref` | Liveness check, sent only after 25s of silence (browsers can't see WebSocket pings) |

**Server → client**

| `t` | Fields | Meaning |
|-----|--------|---------|
| `ok` | `ref`, `data`; for a join also `topic`, `replay` (n), `gap` | Success. A join's `ok` is followed by exactly `replay` `event` frames, then a `presence` `here`. `gap: true` = more was missed than can be replayed: reload the view |
| `err` | `ref`, `code`, `message` | Refused. `code` is stable (for programs), `message` is a sentence (for people) |
| `event` | `topic`, `event`, `data`, `seq`? | A room event. Durable events carry `seq` (the resume cursor); ephemeral ones (typing) don't |
| `presence` | `topic`, `here` **or** `joins` + `leaves` | `here` replaces the list (sent after a join); `joins`/`leaves` patch it. Keys are people (user ids), values minimal metadata |
| `pong` | `ref` | Answer to `ping` |
| `slow` | `topic` | This connection's queue for the topic is ¾ full: it's about to be dropped if it doesn't catch up |
| `bye` | `code`, `reason` | Sent right before the server closes the connection, saying why (`restart`, `revoked`, `too_slow`, `rate_limited`, `protocol`, `limit`, `idle`) |

`err` codes: `forbidden`, `unknown_topic`, `not_joined`, `already_joined`, `invalid` (validation, message = the errors), `rate_limited`, `bad_event` (not in the room's `In` type), `limit`, `read_only`, `unavailable` (room restarting or node unreachable; retry).

Close codes (always preceded by `bye`): `1012` restart · `1013` too slow · `1001` idle · `1008` rate limited / too many connections · `1002` protocol (also: no `offrails.v1` subprotocol) · `1003` binary frame · **`4401` session revoked or expired** (don't reconnect).

**Heartbeats are WebSocket control frames**, not app messages: the server pings every 20s and browsers answer automatically, so a dead client is noticed (no pong in 60s → close). Browsers expose neither pings nor pongs to JavaScript, so the client's only way to notice a dead server is to ask: after 25s without any frame it sends `{"t":"ping"}` and reconnects if no `pong` arrives within 10s.

### A conversation

```text
→ {"t":"join","ref":"1","topic":"book:7","since":41}
← {"t":"ok","ref":"1","topic":"book:7","data":{},"replay":2,"gap":false}
← {"t":"event","topic":"book:7","seq":42,"event":"message","data":{"id":42,"body":"one","author":"AL",…}}
← {"t":"event","topic":"book:7","seq":43,"event":"message","data":{"id":43,"body":"two","author":"BO",…}}
← {"t":"presence","topic":"book:7","here":{"7":{"initials":"AL"}}}
← {"t":"presence","topic":"book:7","joins":{"9":{"initials":"BO"}},"leaves":{}}
→ {"t":"push","ref":"2","topic":"book:7","event":"post","data":{"body":"hi"}}
← {"t":"ok","ref":"2","data":{"id":44}}
← {"t":"event","topic":"book:7","seq":44,"event":"message","data":{"id":44,"body":"hi",…}}
→ {"t":"push","ref":"3","topic":"book:7","event":"post","data":{"body":"   "}}
← {"t":"err","ref":"3","code":"invalid","message":"Body can't be blank"}
← {"t":"bye","code":"revoked","reason":"session revoked"}      (another tab logged out) → close 4401
```

### Resume

`seq` is chosen by the room: a durable, per-topic, increasing number. In the book room it's the message's row id. The client remembers the highest `seq` per topic and rejoins with `since`; the room replays from memory when it covers `since`, else from the database (other nodes always use the database). Replays and live events can overlap (the server subscribes before computing the replay, so nothing is lost in between), so **clients drop events with `seq <= since`**. This is the same model as SSE signals ([realtime.md#resume](realtime.md#resume)), where the cursor is the signal's id.

## Typed channels

A room declares what clients may send and what it emits, as serde-tagged enums. The kernel deserializes every `push` into `Room::In` **before** the room sees it (anything else is `bad_event`), and serializes `Room::Out` into `event` frames:

```rust
// src/channels/book_room.rs
#[derive(Debug, Deserialize)]                            // TS modes: also #[derive(TS)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum BookIn {
    Post(MessageParams),                                 // {"event":"post","data":{"body":"…"}}
}

#[derive(Clone, Debug, Serialize)]                       // TS modes: also #[derive(TS)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum BookOut {
    Message(MessageJson),                                // {"event":"message","data":{…}}
}
```

In TS modes both enums derive `TS` (exported to `web/src/types/`), and `web/src/lib/socket.ts` is generic over them, so a typo in an event name or a field is a `tsc` error on the client and a compile error on the server. In htmx mode `static/channels.js` stays small and untyped but speaks the same protocol.

## The hub (`kernel/hub.rs`)

```rust
pub type Payload = serde_json::Value;
pub type Frame = axum::extract::ws::Utf8Bytes;       // one offrails.v1 frame, serialized once; clones are refcount bumps

pub fn event_frame(topic: &str, seq: Option<i64>, event: &str, data: &Payload) -> Frame;
pub fn presence_here_frame(topic: &str, here: &Payload) -> Frame;
pub fn presence_diff_frame(topic: &str, joins: &[(String, Payload)], leaves: &[(String, Payload)]) -> Frame;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub key: String,                  // the person (user id)
    pub meta: Payload,                // what others see in presence: minimal (initials / display name, never an email)
    #[serde(skip)] pub session: Option<i64>,   // the session this connection authenticated with; revalidated
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection { pub code: String, pub message: String }   // → an `err` frame
pub struct Joined<O> { pub data: Payload, pub replay: Vec<(i64, O)>, pub gap: bool }
pub struct Outbox<O> { /* … */ }
impl<O> Outbox<O> {
    pub fn emit(&mut self, seq: i64, event: O);          // durable: carries `seq`, replayable
    pub fn emit_ephemeral(&mut self, event: O);          // typing, cursors: not replayed
}

pub trait Room: Send + Sized + 'static {
    type Ctx: Clone + Send + Sync + 'static;             // AppState
    type In: DeserializeOwned + Send;                    // what clients may push
    type Out: Serialize + Send;                          // what the room emits
    fn init(ctx: &Self::Ctx, topic: &str) -> impl Future<Output = anyhow::Result<Self>> + Send;
    fn joined(&mut self, ctx: &Self::Ctx, topic: &str, member: &Member, since: Option<i64>)
        -> impl Future<Output = Result<Joined<Self::Out>, Rejection>> + Send;   // owner node, from memory
    fn joined_elsewhere(ctx: &Self::Ctx, topic: &str, member: &Member, since: Option<i64>)
        -> impl Future<Output = Result<Joined<Self::Out>, Rejection>> + Send;   // other nodes, from the DB
    fn handle_in(&mut self, ctx: &Self::Ctx, topic: &str, member: &Member, msg: Self::In, out: &mut Outbox<Self::Out>)
        -> impl Future<Output = Result<Payload, Rejection>> + Send;
}

pub struct Hub<R: Room> { /* Arc<Inner> */ }   // Clone implemented by hand (derive would require R: Clone)
impl<R: Room> Hub<R> {
    pub fn new(node: impl Into<String>) -> Self;
    pub fn subscribe(&self, topic: &str, conn: u64) -> broadcast::Receiver<Frame>;
    pub fn unsubscribe(&self, topic: &str, conn: u64);                    // last one out drops the topic + actor
    pub async fn broadcast(&self, topic: &str, seq: Option<i64>, event: &str, data: &Payload);   // local + every node
    pub async fn track(&self, topic: &str, conn: u64, member: &Member);
    pub async fn untrack(&self, topic: &str, conn: u64);
    pub fn presence_list(&self, topic: &str) -> Payload;
    pub async fn join(&self, ctx: &R::Ctx, topic: &str, member: &Member, since: Option<i64>) -> Result<JoinReply, Rejection>;
    pub async fn handle_in(&self, ctx: &R::Ctx, topic: &str, member: &Member, event: &str, data: Payload) -> Result<Payload, Rejection>;
    pub fn revoke(&self, key: &str);                                      // a session of `key` may have ended
    pub fn watch_revocations(&self, signals: broadcast::Receiver<pubsub::Signal>) -> JoinHandle<()>;
    pub fn conns_of(&self, key: &str) -> usize;                           // per-user caps
    pub fn log_frames(&self, on: bool);                                   // development: /dev/sockets frame log
    pub fn snapshot(&self) -> Snapshot;                                   // for /dev/sockets
    pub fn close(&self);                                                  // shutdown: `bye` restart, close 1012
    pub fn halt(&self);                                                   // tests: die without telling the cluster
}
```

Rules the implementation follows (and tests prove):

- **One actor per topic, one command at a time.** The actor is a tokio task with an `mpsc` mailbox (256). Handlers never race, so a rate limit or a sequence number needs no lock. 200 concurrent `inc` commands on a counter room end at exactly 200.
- **Supervision ("let it crash", per room).** Every `init`/`joined`/`handle_in` runs under `AssertUnwindSafe(..).catch_unwind()`. A panic answers the caller with `unavailable` ("room crashed; try again"), drops the state, counts a restart (shown on `/dev/sockets`), and the next command rebuilds the state with `init`. The mailbox and the connections survive.
- **Serialize once.** `Hub::broadcast` builds one `Frame` and every subscriber receives a clone of the same buffer. The test asserts 1000 receivers share one pointer.
- **Slow consumers are warned, then dropped.** Each topic's channel buffers `TOPIC_BUFFER` (256) frames. At ¾ full the connection gets a `slow` frame; past the buffer it gets `bye` (`too_slow`) and close 1013. The client reconnects and resumes from `since`. One slow phone can't make the server hold memory for everyone.
- **Idle topics are forgotten.** No local connections and nobody present → the topic (and its actor) is dropped. An actor with no commands for 5 minutes stops by itself.
- **Presence lists people, not tabs.** Refs are `(node, conn)`. A person joins when their first ref arrives and leaves when their last ref goes. Two tabs = one avatar.

## The connection (`kernel/channels.rs`)

```rust
pub const PROTOCOL: &str = "offrails.v1";

pub trait Channels: Clone + Send + Sync + 'static {                       // implemented by AppState
    type Room: Room<Ctx = Self>;
    fn hub(&self) -> &Hub<Self::Room>;
    fn socket_config(&self) -> SocketConfig;
    /// Against the database as it is now (also re-checked on every push).
    fn authorize(&self, member: &Member, topic: &str) -> impl Future<Output = Result<(), Rejection>> + Send;
    /// Is this connection's session still valid? Fails closed.
    fn session_valid(&self, member: &Member) -> impl Future<Output = bool> + Send;
}

pub struct SocketConfig {
    pub allowed_origin: String,      // origin of APP_URL
    pub max_frame: usize,            // 4 KiB
    pub max_in_per_10s: u32,         // 100
    pub idle_timeout: Duration,      // 60s without any frame or pong
    pub ping_every: Duration,        // 20s (WebSocket control frames)
    pub revalidate_every: Duration,  // 60s (APP_REALTIME_REVALIDATE_SECS)
    pub max_conns_per_user: usize,   // 128, all transports, per node (a tab can hold 2: size for tabs)
    pub max_topics: usize,           // 20 per connection
    pub max_joins_per_min: u32,      // 30 per connection
}

/// The protocol state of one client, whatever the transport: joined topics, limits, frame logic.
pub struct Session<S: Channels> { /* … */ }
impl<S: Channels> Session<S> {
    pub async fn handle_text(&mut self, raw: &str) -> Next;              // rate limit, parse, dispatch
    pub async fn next_broadcast(&mut self) -> Result<Vec<Frame>, TooSlow>;   // + `slow` warnings
    pub async fn end(self);                                               // leave every topic
}

pub fn upgrade<S: Channels>(s: S, ws: WebSocketUpgrade, headers: &HeaderMap, member: Option<Member>) -> Response;
pub fn sse_open<S: Channels>(s: S, headers: &HeaderMap, member: Option<Member>) -> Response;
pub fn poll_open<S: Channels>(s: S, headers: &HeaderMap, member: Option<Member>) -> Response;
pub async fn poll_get<S: Channels>(s: S, headers: &HeaderMap, member: Option<Member>, conn: u64) -> Response;
pub async fn relay_post<S: Channels>(s: S, headers: &HeaderMap, member: Option<Member>, conn: u64, body: String) -> Response;
pub fn session_ended<S: Channels>(s: S, member: Member) -> impl Future<Output = ()> + Send;   // shared by every transport
```

All three transports drive the same `Session`, so joins, authorization, limits, resume and revocation are identical everywhere. Per WebSocket, one task `select!`s over: incoming frames, the joined topics' receivers, the ping timer, the idle check, the revalidation timer, the member's revocation signal, and `hub.closed()`.

| Situation | What happens |
|-----------|--------------|
| `Origin` missing or not exactly APP_URL's origin | **403** at the handshake (CSWSH: browsers send cookies on cross-site WebSocket handshakes, and CSRF protection doesn't cover them) |
| Logged out | **401** at the handshake |
| No `offrails.v1` subprotocol offered | the socket opens, gets `bye` `protocol` "subprotocol offrails.v1 required", closes **1002** (browsers hide an HTTP refusal's reason). A client offering only other subprotocols can't connect at all |
| More than `max_conns_per_user` | `bye` `limit`, close **1008** (HTTP transports: 429) |
| Join | limits (`max_topics`, `max_joins_per_min`), `authorize`, then **subscribe before computing the reply** (nothing broadcast in between is lost), `ok` with `replay` n, the replayed `event`s, `presence` `here`, then `track` |
| Push | `not_joined` unless joined; **re-`authorize`** (a reader removed since joining is refused and leaves the topic); the room's `In` type (`bad_event`); then the room |
| More than `max_in_per_10s` frames | `bye` `rate_limited`, close **1008** |
| Not JSON / an array / unknown `t` / binary | `bye` `protocol`, close **1002** / **1003** |
| Frame over `max_frame` | tungstenite rejects it, and the socket closes |
| No pong (or anything) for `idle_timeout` | close **1001** |
| Logout, password change, revocation, expiry | `bye` `revoked`, close **4401** ([Security](#security)) |
| Shutdown (`hub.close()`) | `bye` `restart`, close **1012**, so clients reconnect, possibly to another node |

## Transports

A client tries the WebSocket first and falls back automatically:

| Order | Transport | Down | Up | Gets through |
|-------|-----------|------|----|--------------|
| 1 | **WebSocket** (default) | `GET /socket` (offrails.v1) | same socket | most networks |
| 2 | **SSE + POST** | `GET /socket/sse`: event `hello` `{conn, token}`, then `frame` events (one frame each) | `POST /socket/sse/{conn}` (one frame, header `x-offrails-token`) → 202 | almost every proxy (plain HTTP) |
| 3 | **Long-polling** | `POST /socket/poll` → `{conn, token}`; `GET /socket/poll/{conn}` waits ≤ 25s, returns a JSON array of frames | `POST /socket/poll/{conn}` → 202 | anywhere HTTP works |

- **Choosing:** the client uses the transport that worked last in this tab (`sessionStorage`), else the first. A transport that doesn't open within 5s, or closes before opening, counts as blocked: the client moves to the next one.
- **Upgrading back:** on a fallback, the client probes the WebSocket every 60s; when it opens, it switches over and rejoins (resuming from `since`).
- **Same guarantees:** relays (SSE, long-poll) are `Session`s like sockets. Replies to posted frames come down the stream or the next poll. A relay is bound to the member who opened it **and** a random 256-bit token (compared in constant time): another user, or the same user without the token, gets 403. POSTs pass the app's CSRF layer; GETs refuse a foreign `Origin` or `Sec-Fetch-Site: cross-site`. Body limit = `max_frame` (413); a full inbox = 429; an ended relay = 410 after its `bye` was delivered.
- **`APP_REALTIME_TRANSPORTS`** (default `websocket,sse,longpoll`) lists what the server offers. Behind a proxy that mishandles WebSockets, drop `websocket`: the endpoint answers 404 like a blocking proxy, and clients fall back. The Playwright spec uses this to force SSE, then blocks `/socket/sse` with `context.route` to force long-polling.

## Security

Realtime connections are long-lived and authenticated once, so every rule a request gets for free has to be enforced for the connection's whole life. **Security is the gold standard here; each rule has a test** (`tests/app/wire_test.rs`, `kernel_pubsub_test.rs`).


Every transport (the WebSocket upgrade, `/socket/sse` + POST, and long-polling) authenticates through the same session extractor as ordinary requests (`load_session`, via `SessionAndUser`). So all of them inherit the production `__Host-session_token` name and ignore a planted plain `session_token`, and their responses pass through the security middleware (`no-store`, the full header matrix; [security](security.md#the-header-matrix)). Never parse the session cookie by hand in realtime code.

| Rule | How | Test |
|------|-----|------|
| **Same-origin only, every transport** | WebSocket: `Origin` must equal APP_URL's origin (missing = refused). SSE / long-poll GETs: refuse a foreign `Origin` or `Sec-Fetch-Site: cross-site`. POSTs: the CSRF layer. Session cookies are `SameSite=Lax` too | cross-site Origin on WebSocket, SSE stream, SSE relay, long-poll, relay POST |
| **Logout, password change, revocation close connections at once** | `session::destroy` / `destroy_all_for_user` write a `member:<id>` / `session-ended` signal (pubsub, so every process and node sees it). The hub wakes that member's connections; each re-checks **its own** session: the revoked one gets `bye` `revoked` + **4401**, the same person's other devices stay | logout closes that session's sockets only; password change closes all; logout ends SSE streams with `bye`; logout ends relays |
| **Expiry closes connections** | every `revalidate_every` (60s, `APP_REALTIME_REVALIDATE_SECS`) each connection calls `session_valid` (fails closed) | expired session closed by revalidation (socket and SSE) |
| **Authorization against current state** | joins and **every push** call `authorize` against the database now; a refused push also leaves the topic | push after the book is deleted → `forbidden`, then `not_joined` |
| **Per-user caps** | ≤ 128 connections per member (all transports, per node; each tab can hold 2, and a cap of 8 broke users at ~5 tabs, see [realtime](realtime.md#many-tabs-stall-http11-vs-http2)), ≤ 20 topics per connection, ≤ 30 joins per minute per connection, ≤ 100 frames per 10s, frames ≤ 4 KiB | each limit |
| **Presence leaks nothing** | only members of the topic (who passed `authorize`) receive its presence; `meta` is minimal: initials or a display name, **never an email** unless the spec says so | no `@` in presence frames |
| **Relays can't be hijacked** | `{conn, token}`: owner member + 256-bit token, constant-time comparison | wrong token, other member, unknown conn |
| **Untrusted input stays data** | typed `In` enums (unknown events refused before the room), text rendered with `textContent` | `bad_event`; HTML delivered raw, rendered escaped |

The client reacts to `bye` `revoked` by **not reconnecting** (it would only loop into 401s) and fires `offrails:revoked` on `window`; the page can redirect to the login form.

## The app side

```rust
// src/channels/mod.rs
pub async fn socket(State(s): State<AppState>, SessionAndUser(found): SessionAndUser, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !offered(&s, "websocket") { return StatusCode::NOT_FOUND.into_response(); }
    kchannels::upgrade(s, ws, &headers, member_of(&found))
}
// socket_sse, socket_poll_open, socket_poll, socket_send: the same, calling sse_open / poll_open / poll_get / relay_post.

pub fn member_for(u: &User, session: Option<i64>) -> Member {
    Member { key: u.id.to_string(), meta: json!({ "initials": user::initials(&u.email) }), session }
}

/// Topics: `book:{id}` (presence + discussion) and `user:{id}` (private notices).
pub enum AppRoom { Book(BookRoom), Private }

impl Room for AppRoom { type Ctx = AppState; type In = BookIn; type Out = BookOut; /* dispatch on the topic prefix */ }

impl Channels for AppState {
    type Room = AppRoom;
    fn hub(&self) -> &Hub<AppRoom> { &self.hub }
    fn socket_config(&self) -> SocketConfig { /* SocketConfig::new(app_url) + revalidate_every from config */ }
    async fn authorize(&self, member: &Member, topic: &str) -> Result<(), Rejection> { /* user:{id} → own id only; book:{id} → books::authorize, against the DB now */ }
    async fn session_valid(&self, member: &Member) -> bool { /* session::is_live(db, member.session) */ }
}
```

- **Routes** (with `ROUTES` rows, `Access::User`): `GET /socket`, `GET /socket/sse`, `POST /socket/sse/{conn}`, `POST /socket/poll`, `GET`+`POST /socket/poll/{conn}`. Keep the poll routes explicit: a POST to `/socket/poll/1` matching a GET-only static route answers 405 instead of falling through to a `{transport}` param route.
- **Extractor:** `SessionAndUser` (session + user), so the connection knows which session to revalidate.
- **State:** `hub: Hub::new(node)`, `hub.log_frames(config.is_development())`, `hub.watch_revocations(pubsub.subscribe())` in `AppState::new`.
- **`serve`:** `kernel::cluster::start(..)` when `config.cluster`, and `hub.close()` next to `pubsub.close()` in the graceful-shutdown future.
- **A room** (`src/channels/book_room.rs`) keeps the newest 50 messages in a `VecDeque` and knows whether that covers a client's `since`; it goes to the database only when it doesn't. `handle_in(BookIn::Post)`:
  1. per-reader rate limit (10 per 10s, `rate_limited` "Slow down"), consistent across tabs and nodes because only the owner's actor sees posts;
  2. validate (`MessageParams::validate`, the same `Errors` as forms; `invalid`);
  3. **insert**;
  4. push to memory;
  5. `out.emit(row.id, BookOut::Message(json))`;
  6. reply `{id}`.
- **The page** renders the newest 50 messages server-side, escaped by Askama, so the first paint needs no socket. The client joins with `since = max(data-id)` and gets only newer messages.

## The JS client

`static/channels.js` (htmx mode; loaded in the layout before `app.js`) defines `window.OffRails = { Connection, Presence, transports, socket() }`. TS modes put the same code, typed by the room's exported `In`/`Out` types, in `web/src/lib/socket.ts`.

```js
const ch = window.OffRails.socket().channel("book:7", { since: lastSeen });   // the cursor advances with every `seq`
ch.on("message", add);                                   // typed Out event
ch.onJoin(() => (error.textContent = ""));
ch.on("gap", () => location.reload());                   // missed more than the room can replay
ch.onError((e) => (error.textContent = e.message));      // {code, message}
new window.OffRails.Presence(ch).onSync((people) => render(people));          // [{key, initials}]
form.addEventListener("submit", (e) => { e.preventDefault(); ch.push("post", { body }).then(clear).catch((e) => show(e.message)); });
ch.join();
addEventListener("offrails:revoked", () => (location.href = "/session/new"));  // optional
```

- **One connection per tab** (`OffRails.socket()`), many channels, any transport.
- **Reconnect** with backoff 250ms → 1s → 2s → 5s → 10s; every wanted channel rejoins with its current `since`.
- **`join()` before the connection is open just waits.**
- **Replies are promises** with a 10s timeout; rejections are `{code, message}`.
- **Render user text with `textContent`, never `innerHTML`.**
- **htmx pages:**
  - start rooms on `DOMContentLoaded` and `htmx:load`;
  - on `htmx:beforeSwap` with `detail.boosted`, leave the page's channels, so presence updates for everyone else when you navigate away;
  - put **`hx-boost="false"` on any form the socket sends**: a boosted form submit also navigates (to `?body=…`), which the browser check caught.

## More than one server (`kernel/cluster.rs`)

**Staging rule.** Pick the first stage that fits and move on only when the spec needs it:

1. **One node** (the default; SQLite or Postgres). Everything is in memory in one process. One Rust server holds tens of thousands of sockets.
2. **Several stateless nodes behind a load balancer** (Postgres). Jobs, cache and SSE signals already work across nodes through the database (`NOTIFY` wakes every node). No cluster layer.
3. **The cluster layer** (`APP_CLUSTER=true`, Postgres only). Add it only when channels with rooms or presence must span nodes.

The cluster layer is **the database as the coordinator**, no new infrastructure:

- **Membership:** each node upserts `cluster_nodes(node, started_at, heartbeat_at)` every `heartbeat` (2s) and reads everyone fresher than `dead_after` (6s). The node name is `APP_NODE`, defaulting to `<hostname>:<pid>`.
- **Messages:** `pg_notify('offrails_cluster', json)`, received by a `PgListener` per node. Each message is `Fanout`, `Presence`, `Hello` (a new node asks for everyone's presence), `Call` or `Reply`.
- **Ownership:** rendezvous (highest random weight) hashing of `(node, topic)` with FNV-1a. It must be a fixed hash, because std's hasher is seeded per process and nodes would disagree. Every node computes the same owner from the same member list, and when a node leaves, only its topics move (the test checks that about 1/3 move when one of three leaves).
- **`handle_in` on a non-owner** becomes a `Call` to the owner, which replies with `Reply` within `call_timeout` (3s). If the call fails and the topic's owner changed meanwhile (the owner died), it's retried once against the new owner.
- **Joins on a non-owner** use `Room::joined_elsewhere`, which reads from the database. Replays can be larger than a NOTIFY allows.
- **Presence:** each node announces its own refs. A node marked dead is removed from every topic, and the other nodes send `presence` `leaves`.
- **Revocations** need no cluster message: they're pubsub signals (`member:<id>`), which every node already receives.
- **Failover:** a dead owner's topics move to survivors. Their actor `init`s from the database. Clients that lost their node reconnect (1012 or a dropped TCP connection) to any node and resume from `since`.
- **Graceful leave:** on `hub.close()` the node deletes its `cluster_nodes` row, so the others react on their next heartbeat instead of after `dead_after`.

**Why NOTIFY instead of node-to-node HTTP:**
- no internal port, shared secret, or private-network setup to get wrong on each host;
- it works anywhere the app can reach its database.

The costs:
- payloads are capped at 8,000 bytes (`MAX_NOTIFY` = 7,900). That's why socket frames are capped at 4 KiB and replays never cross nodes;
- throughput is bounded by Postgres, comfortably thousands of messages per second.

If a product outgrows that, the escape hatch is an HTTP mesh between nodes, logged in `docs/decisions.md`.

**Not provided** (and not needed for a web app): process ids that work across nodes, hot code reload, and supervision across nodes.

## `/dev/sockets`

A kernel page in the `/dev` tab bar (same rules as every kernel page: own layout, `no-store`, `hx-boost="false"` links into it). It polls every 2s and shows:
- this node's name, connections, topics, frames in and out, and the rate since the last poll;
- room restarts, flagged in red if any;
- live nodes;
- per topic: local connections, people present, owner, whether its actor is running, and broadcasts sent;
- per connection: member, **transport** (`websocket`, `sse`, `longpoll`), topics, age, frames in and out;
- **a live decoded frame log**: the last 40 frames per connection, in (↑) and out (↓), as readable offrails.v1 JSON. Development only (`Hub::log_frames`): nothing is copied or kept in production.

## Measured (reference app, M-series laptop under heavy parallel load, 2026-10-12)

| What | Result |
|------|--------|
| Fan-out, 1,000 in-process subscribers (send → last receiver woken) | p50 **1.3 ms** |
| Fan-out, 10,000 subscribers | p50 **12 ms** |
| WebSocket round trip, localhost (`ping` → `pong`) | p50 **66 µs**, p90 91 µs |
| SSE signal latency, SQLite, before (250ms poll) → after (in-process wake) | **238 ms → 0.24 ms** |
| SSE signal latency, Postgres, before → after (`NOTIFY`) | **238 ms → 2.1 ms** |

Run them with `cargo nextest run --run-ignored only -E 'test(bench_)' --no-capture` (and with `TEST_DATABASE_URL` for Postgres).

## Testing

`tests/app/support.rs`:
- **`serve(state) -> (addr, JoinHandle)`:** sockets need a real port; abort the handle to kill a node.
- **`session_cookie(db, user)`** / **`session_for(db, user) -> (session id, cookie)`:** one browser per user; the id lets a test end exactly that session.
- **`Ws`**, an offrails.v1 client:
  - `Ws::connect(addr, cookie, origin) -> Result<Ws, StatusCode>` (offers `offrails.v1`; surfaces handshake 401/403), `connect_with_protocol(.., None | Some("other"))`, `Ws::as_user(addr, cookie)`;
  - `join(topic, since) -> Result<JoinedTopic {data, replay, gap}, Rejected {code, message}>` (collects the replayed events after the `ok`);
  - `push(topic, event, data) -> Result<data, Rejected>`, `leave(topic)`, `request(frame)`;
  - `event(topic, event)`, `presence(topic)`, `recv_where(pred)` (keeps non-matching frames for later), `quiet(pred, ms)`;
  - `closed() -> (close code, bye code)`, `closed_with() -> close code`.
- **Relays:** `open_stream("/socket/sse")` + `next_sse_data` reads `hello` and `frame` events; post frames with a same-origin request carrying `x-offrails-token`.

Required cases, per channel:
- **join authorization:** logged out gets 401, a foreign Origin gets 403, and someone else's private topic, an unknown topic, or a missing record each give `forbidden` / `unknown_topic`;
- **delivery:** a message reaches others and is stored, `seq` equals its id, and validation errors carry `invalid` + the reason; an undeclared event is `bad_event`;
- **presence:** `here` then `joins`/`leaves`, two tabs counting as one person, no emails in metadata;
- **resume:** after a reconnect with `since`, exactly the missed events arrive, in order, from memory and from the database;
- **limits:** the per-room rate limit, an oversized frame, flooding (1008), garbage / unknown `t` / legacy arrays (1002);
- **output:** HTML in messages is delivered raw and rendered escaped.

Protocol and security cases (`wire_test.rs`, once per app): subprotocol required (1002 + `bye`) and other subprotocols refused; unknown fields ignored; `ping`/`pong`; connection, topic and join caps; logout closes that session's sockets only (4401), password change closes all, expiry closes by revalidation; pushes re-authorized after access is lost; SSE + POST and long-polling speak the same protocol (and talk to WebSocket clients in the same room); relays need owner + token + same origin; logout ends relays; cross-site refused on every transport.

Kernel cases (`kernel_channels_test.rs`):
- **supervision:** a panic gets `unavailable`, `init` runs again, and the restart is counted;
- one command at a time; typed `In` refuses undeclared events;
- serialize-once (pointer equality);
- presence counting and idle topic cleanup;
- **ownership:** owners are stable, and a leave moves only that node's topics;
- **signals:** a committed signal is delivered with the backstop poll set to 60s, and a rolled-back one never is;
- **the 3-node cluster test** (Postgres only): presence crosses nodes, a message on the owner reaches a socket on another node, `halt()` the owner, presence leave, posting on the new owner, the reconnecting client resumes from a third node, each message is stored once, and jobs drained by two nodes run exactly once.

Real browser (`tests/e2e/realtime.spec.ts`, Playwright; the spec starts and stops its own servers by handle):
- **downtime:** a page reached by a boosted form, its server stopped, a review changed and a chat message posted through a second process, the server restarted: the open page shows both without a reload;
- **fallback:** with `APP_REALTIME_TRANSPORTS=sse,longpoll` the page runs on SSE (presence, chat both ways); with `/socket/sse` blocked too, on long-polling.

## Gotchas agents get wrong

- **Checking `Origin` only when present** on the WebSocket. A missing `Origin` must be refused too: browsers always send it on WebSocket handshakes.
- **Broadcasting before writing.** If the insert fails, everyone already saw a message that doesn't exist. Write, then `emit`.
- **Resume cursors that aren't durable.** `seq` must survive restarts and node moves (a row id), not an in-memory counter.
- **Filtering replays by "id <= cursor" on the server.** Postgres sequences can commit out of order; dedupe on the client by `seq` and replay exact ids.
- **Subscribing lazily.** Subscribe to the topic before computing the join reply, and to revocations before the first await, or what happens in between is lost (the browser check found a logout missed this way).
- **Replaying from the actor's memory on every node.** Only the owner has it. Elsewhere use `joined_elsewhere` (database).
- **Sending replays through `NOTIFY`.** 8,000-byte cap. Replies over the cluster must stay small.
- **Using `std::collections::hash_map::DefaultHasher` for ownership.** It's seeded per process. Use the fixed FNV-1a in `cluster.rs`.
- **`#[derive(Clone)]` on `Hub<R>`.** That requires `R: Clone`. Implement `Clone` by hand.
- **`async fn` in a trait used with `tokio::spawn`.** Declare `-> impl Future<Output = …> + Send` in the trait. `async fn` in the impl is fine.
- **Timing assertions.** `elapsed() < 150ms` passed alone and failed under parallel load. Prove a fast path by making the slow path impossible (`start_with(60s)`), not by racing a clock.
- **A dying node announcing its leaves.** A crash says nothing; the others learn it from heartbeats. `halt()` publishes nothing, so tests exercise the real path.
- **Raw `PgListener::recv()` inside a `select!` with other branches.** If another branch wins, the `recv` future is dropped mid-read. Give the listener its own task and forward over a channel ([realtime.md](realtime.md)).
- **No `hx-boost="false"`** on the socket's form (see the JS client section).
- **Treating an `EventSource` reopen as the same connection** on the SSE transport: a reopen is a *new* server session with no joins. Close it and reconnect through the client, which rejoins.
- **Relay ids without a token**, or a token compared with `==` on secret material: bind relays to member + token, compare in constant time.
- **`Result<_, Response>` in kernel helpers:** clippy's `result_large_err`; return a `StatusCode` and convert at the edge.

