# Channels, Presence and the Hub (Phoenix Channels → `kernel/hub.rs` + WebSockets)

[realtime.md](realtime.md) covers **signals**: "book 42's reviews changed, re-fetch". That's enough for live lists, counters and notifications, and it stays the default. Channels are for what signals can't do: **the client talks back** (chat, typing, games, collaborative tools) or the page needs **who's here** (presence). They borrow Phoenix's design: one multiplexed WebSocket per tab, topics you join with authorization, a state-owning process per room, and presence that's correct across nodes.

> Verified on 2026-10-12 in the reference app (htmx, "who's reading" avatars and a per-book discussion): axum 0.8.9 `ws`, tokio 1.53, sea-orm 2.0.4 with sqlx 0.9's `PgListener`, tokio-tungstenite 0.29 as the test client. 21 tests on SQLite and Postgres 18, a 3-node cluster test with a node killed mid-conversation, a two-browser Playwright smoke, and a manual Chrome session that included a server restart.

| Rails / Phoenix | Off Rails |
|-----------------|-----------|
| Action Cable channel / Phoenix.Channel | `src/channels/<topic>_room.rs`: a `Room` impl, plus an arm in `AppRoom` and `authorize` |
| Phoenix.Socket (one socket, many topics) | `GET /socket`, `kernel/channels.rs` |
| GenServer per room, `handle_in/3` | `kernel/hub.rs` room actor: one tokio task per topic, one command at a time |
| Phoenix.PubSub | the hub's per-topic `broadcast` channel (in-process) + `NOTIFY` between nodes |
| Phoenix.Presence | `Hub::track` / `untrack`; `presence_state` + `presence_diff` events |
| Registry / Horde / libcluster | `kernel/cluster.rs`: `cluster_nodes` heartbeats + rendezvous hashing over Postgres |
| phoenix.js | `static/channels.js` (htmx) / `web/src/lib/socket.ts` (TS modes) |
| LiveDashboard sockets page | `/dev/sockets` |

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
browser tab ──WebSocket /socket──▶ kernel/channels.rs   (auth at upgrade, Origin check, limits, wire protocol)
                                        │ join / handle_in
                                        ▼
                                  kernel/hub.rs  Hub<AppRoom>   (one per process, in AppState)
                                  ├─ topic "book:7": broadcast::Sender<Frame>  → every local socket on it
                                  ├─ presence {person → refs (node, conn)}
                                  └─ room actor (tokio task) owns AppRoom::Book(BookRoom) state
                                        │ durable writes first
                                        ▼
                                  database (book_messages)  ← init() rebuilds state from here
                    (cluster only)  kernel/cluster.rs: cluster_nodes heartbeat + LISTEN/NOTIFY "offrails_cluster"
```

**Memory first, database as truth.** A room's state is memory, so a join, a replay or a rate-limit check costs microseconds. Anything that must survive (messages) is written to the database **before** it's broadcast, so `Room::init` can rebuild the state after a crash, a restart, or a move to another node. Presence is never stored: clients re-announce it by rejoining.

## The wire protocol (Phoenix v2)

Every frame, both directions, is a JSON array: `[join_ref, ref, topic, event, payload]`.

| Client sends | Server answers |
|--------------|----------------|
| `[jr, r, "book:7", "phx_join", {"last_seen": 41}]` | `[jr, r, "book:7", "phx_reply", {"status": "ok", "response": {...}}]`, then `presence_state` |
| `[jr, r, "book:7", "new_msg", {"body": "hi"}]` | `phx_reply` with `ok` + response, or `error` + `{"reason": "Slow down"}` |
| `[jr, r, "book:7", "phx_leave", {}]` | `phx_reply` ok |
| `[null, r, "phoenix", "heartbeat", {}]` (every 30s) | `phx_reply` ok |
| | broadcasts: `[null, null, "book:7", "new_msg", {...}]`, `presence_diff` `{"joins": {key: meta}, "leaves": {key: meta}}` |

Rejection reasons are plain sentences shown to the user ("not allowed", "Body can't be blank", "join the topic first").

## The hub (`kernel/hub.rs`)

```rust
pub type Payload = serde_json::Value;
pub type Frame = axum::extract::ws::Utf8Bytes;       // serialized once; clones are refcount bumps

pub fn frame(join_ref: Option<&str>, msg_ref: Option<&str>, topic: &str, event: &str, payload: &Payload) -> Frame;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member { pub key: String, pub meta: Payload }   // key = the person (user id); meta = what others see

#[derive(Default)]
pub struct Outbox { /* … */ }
impl Outbox { pub fn broadcast(&mut self, event: &str, payload: Payload); }  // sent after the handler returns

pub trait Room: Send + Sized + 'static {
    type Ctx: Clone + Send + Sync + 'static;                // AppState
    fn init(ctx: &Self::Ctx, topic: &str) -> impl Future<Output = anyhow::Result<Self>> + Send;
    fn joined(&mut self, ctx: &Self::Ctx, topic: &str, member: &Member, params: &Payload)
        -> impl Future<Output = Result<Payload, String>> + Send;          // owner node, from memory
    fn joined_elsewhere(ctx: &Self::Ctx, topic: &str, member: &Member, params: &Payload)
        -> impl Future<Output = Result<Payload, String>> + Send;          // other nodes, from the DB
    fn handle_in(&mut self, ctx: &Self::Ctx, topic: &str, member: &Member, event: &str, payload: Payload, out: &mut Outbox)
        -> impl Future<Output = Result<Payload, String>> + Send;
}

pub struct Hub<R: Room> { /* Arc<Inner> */ }   // Clone implemented by hand (derive would require R: Clone)
impl<R: Room> Hub<R> {
    pub fn new(node: impl Into<String>) -> Self;
    pub fn subscribe(&self, topic: &str, conn: u64) -> broadcast::Receiver<Frame>;
    pub fn unsubscribe(&self, topic: &str, conn: u64);                    // last one out drops the topic + actor
    pub fn local_fanout(&self, topic: &str, frame: Frame) -> usize;
    pub async fn broadcast(&self, topic: &str, event: &str, payload: &Payload);   // local + every node
    pub async fn track(&self, topic: &str, conn: u64, member: &Member);
    pub async fn untrack(&self, topic: &str, conn: u64);
    pub fn presence_list(&self, topic: &str) -> Payload;
    pub async fn join(&self, ctx: &R::Ctx, topic: &str, member: &Member, params: Payload) -> Result<Payload, String>;
    pub async fn handle_in(&self, ctx: &R::Ctx, topic: &str, member: &Member, event: &str, payload: Payload) -> Result<Payload, String>;
    pub fn snapshot(&self) -> Snapshot;                                   // for /dev/sockets
    pub fn close(&self);                                                  // shutdown: sockets close 1012
    pub fn halt(&self);                                                   // tests: die without telling the cluster
}
```

Rules the implementation follows (and tests prove):

- **One actor per topic, one command at a time.** The actor is a tokio task with an `mpsc` mailbox (256). Handlers never race, so a rate limit or a sequence number needs no lock. 200 concurrent `inc` commands on a counter room end at exactly 200.
- **Supervision ("let it crash", per room).** Every `init`/`joined`/`handle_in` runs under `AssertUnwindSafe(..).catch_unwind()`. A panic answers the caller with `"room crashed; try again"`, drops the state, counts a restart (shown on `/dev/sockets`), and the next command rebuilds the state with `init`. The mailbox and the sockets survive.
- **Serialize once.** `Hub::broadcast` builds one `Frame` and every subscriber receives a clone of the same buffer. The test asserts 1000 receivers share one pointer.
- **Slow consumers are dropped.** Each topic's channel buffers `TOPIC_BUFFER` (256) frames. A socket that falls further behind gets `Lagged`, and the socket closes with 1013. The client reconnects and replays from `last_seen`. One slow phone can't make the server hold memory for everyone.
- **Idle topics are forgotten.** No local sockets and nobody present → the topic (and its actor) is dropped. An actor with no commands for 5 minutes stops by itself.
- **Presence lists people, not tabs.** Refs are `(node, conn)`. A person joins when their first ref arrives and leaves when their last ref goes. Two tabs = one avatar.

## The socket (`kernel/channels.rs`)

```rust
pub trait Channels: Clone + Send + Sync + 'static {                       // implemented by AppState
    type Room: Room<Ctx = Self>;
    fn hub(&self) -> &Hub<Self::Room>;
    fn socket_config(&self) -> SocketConfig;
    fn authorize(&self, member: &Member, topic: &str) -> impl Future<Output = Result<(), String>> + Send;
}

pub struct SocketConfig { pub allowed_origin: String, pub max_frame: usize, pub max_in_per_10s: u32, pub idle_timeout: Duration }
impl SocketConfig { pub fn new(app_url: &str) -> Self }    // origin of APP_URL, 4 KiB, 100 per 10s, 60s

/// The app's handler extracts the member from its session and calls this.
pub fn upgrade<S: Channels>(s: S, ws: WebSocketUpgrade, headers: &HeaderMap, member: Option<Member>) -> Response;
```

Per socket, one task `select!`s over: the socket's incoming frames, a `StreamMap<topic, BroadcastStream<Frame>>` of joined topics, a 5s idle check, and `hub.closed()`.

| Situation | What happens |
|-----------|--------------|
| `Origin` missing or not exactly APP_URL's origin | **403** at the handshake (CSWSH: browsers send cookies on cross-site WebSocket handshakes, and CSRF protection doesn't cover them) |
| Logged out | **401** at the handshake |
| Join | `authorize`, then **subscribe before computing the reply** (nothing broadcast in between is lost), reply, `presence_state`, then `track` |
| Unknown/forbidden topic | `phx_reply` error: `"unknown topic"` / `"not allowed"` |
| Event on a topic not joined | `"join the topic first"` |
| Frame over `max_frame` | tungstenite rejects it, and the socket closes |
| More than `max_in_per_10s` frames | close **1008** (policy) |
| Not JSON / not a 5-array / binary | close **1002** (protocol) / **1003** |
| Nothing received for `idle_timeout` | close **1001** |
| Shutdown (`hub.close()`) | close **1012** (service restart), so clients reconnect, possibly to another node |

## The app side

```rust
// src/channels/mod.rs
pub async fn socket(State(s): State<AppState>, CurrentUser(user): CurrentUser, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    kchannels::upgrade(s, ws, &headers, user.as_ref().map(member_for))
}

pub fn member_for(u: &User) -> Member {
    Member { key: u.id.to_string(), meta: json!({ "initials": user::initials(&u.email) }) }
}

/// Topics: `book:{id}` (presence + discussion) and `user:{id}` (private notices).
pub enum AppRoom { Book(BookRoom), Private }

impl Room for AppRoom { type Ctx = AppState; /* dispatch on the topic prefix to each room */ }

impl Channels for AppState {
    type Room = AppRoom;
    fn hub(&self) -> &Hub<AppRoom> { &self.hub }
    fn socket_config(&self) -> SocketConfig { SocketConfig::new(&self.config.app_url) }
    /// Same rules as the pages: you can join a book's room if you can see the book.
    async fn authorize(&self, member: &Member, topic: &str) -> Result<(), String> { /* user:{id} → own id only; book:{id} → books::authorize */ }
}
```

- **Route:** `.route("/socket", get(channels::socket))`, with `("GET", "/socket", "channels::socket", Access::User)` in `ROUTES` and `paths::socket()`.
- **State:** `hub: Hub::new(config.node.clone().unwrap_or_else(node_id))` in `AppState::new`.
- **`serve`:** `kernel::cluster::start(..)` when `config.cluster`, and `hub.close()` next to `pubsub.close()` in the graceful-shutdown future.
- **A room** (`src/channels/book_room.rs`) keeps the newest 50 messages in a `VecDeque`. It knows whether that covers a client's `last_seen`, and goes to the database only when it doesn't. `handle_in("new_msg")`:
  1. per-reader rate limit (10 per 10s, `"Slow down"`), consistent across tabs and nodes because only the owner's actor sees posts;
  2. validate (`MessageParams::validate`, the same `Errors` as forms);
  3. **insert**;
  4. push to memory;
  5. `out.broadcast("new_msg", json)`;
  6. reply `{id}`.
- **Replay:** join params carry `last_seen` (the highest message id the page has). The reply is `{messages, truncated}`, oldest first, at most 500. The client drops any message with `id <= last_seen`, because a live broadcast and the replay can both carry the same message.
- **The page** renders the newest 50 messages server-side, escaped by Askama, so the first paint needs no socket. The client then joins with `last_seen = max(data-id)` and fills in anything newer.

## The JS client

`static/channels.js` (htmx mode; loaded in the layout before `app.js`) defines `window.OffRails = { Socket, Presence, socket() }`. TS modes put the same code, typed, in `web/src/lib/socket.ts` and export the classes instead of a global.

```js
const ch = window.OffRails.socket().channel("book:7", () => ({ last_seen: lastSeen }));  // params re-read on every rejoin
ch.on("new_msg", add);
ch.onJoin((r) => { error.textContent = ""; r.messages.forEach(add); });
ch.on("join_error", (reason) => (error.textContent = reason));
new window.OffRails.Presence(ch).onSync((people) => render(people));                    // [{key, initials}]
form.addEventListener("submit", (e) => { e.preventDefault(); ch.push("new_msg", { body }).then(clear).catch(show); });
ch.join();
```

- **One socket per tab** (`OffRails.socket()`), many channels.
- **Reconnect** with backoff 100ms → 500ms → 1s → 2s → 5s. `onopen` rejoins every wanted channel with fresh `params()`.
- **`join()` before the socket is open just waits.** Rejecting there showed a stuck "disconnected" error in the browser.
- **Replies are promises** with a 10s timeout. Heartbeat every 30s.
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
- **Presence:** each node announces its own refs. A node marked dead is removed from every topic, and the other nodes emit `presence_diff` leaves.
- **Failover:** a dead owner's topics move to survivors. Their actor `init`s from the database. Clients that lost their node reconnect (1012 or a dropped TCP connection) to any node and replay from `last_seen`.
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
- per topic: local sockets, people present, owner, whether its actor is running, and broadcasts sent;
- per connection: member, topics, age, frames in and out.

## Measured (reference app, M-series laptop under heavy parallel load, 2026-10-12)

| What | Result |
|------|--------|
| Fan-out, 1,000 in-process subscribers (send → last receiver woken) | p50 **1.3 ms** |
| Fan-out, 10,000 subscribers | p50 **12 ms** |
| WebSocket round trip, localhost (heartbeat → reply) | p50 **66 µs**, p90 91 µs |
| SSE signal latency, SQLite, before (250ms poll) → after (in-process wake) | **238 ms → 0.24 ms** |
| SSE signal latency, Postgres, before → after (`NOTIFY`) | **238 ms → 2.1 ms** |

Run them with `cargo nextest run --run-ignored only -E 'test(bench_)' --no-capture` (and with `TEST_DATABASE_URL` for Postgres).

## Testing

`tests/app/support.rs`:
- **`serve(state) -> (addr, JoinHandle)`:** sockets need a real port; abort the handle to kill a node.
- **`session_cookie(db, user)`:** one browser per user.
- **`Ws`**, a protocol-speaking client:
  - `Ws::connect(addr, cookie, origin) -> Result<Ws, StatusCode>`, which surfaces handshake 401/403;
  - `Ws::as_user(addr, cookie)`;
  - `join(topic, params)` and `push(topic, event, payload) -> Result<response, reason>`;
  - `event(topic, event)`;
  - `recv_where(pred)`, which keeps non-matching frames for later;
  - `quiet(topic, event, ms)`;
  - `closed_with() -> close code`.

Required cases, per channel:
- **join authorization:** logged out gets 401, a foreign Origin gets 403, and someone else's private topic, an unknown topic, or a missing record each give an error reply;
- **delivery:** a message reaches others and is stored, and validation errors carry their reason;
- **presence:** join and leave, plus two tabs counting as one person;
- **replay:** after a reconnect, exactly the missed messages arrive, in order;
- **limits:** the per-room rate limit, an oversized frame, flooding (1008) and garbage (1002);
- **output:** HTML in messages is delivered raw and rendered escaped.

Kernel cases (in the kernel test file):
- **supervision:** a panic gets a crash reply, `init` runs again, and the restart is counted;
- one command at a time;
- serialize-once (pointer equality);
- presence counting and idle topic cleanup;
- **ownership:** owners are stable, and a leave moves only that node's topics;
- **signals:** a committed signal is delivered with the backstop poll set to 60s, and a rolled-back one never is;
- **the 3-node cluster test** (Postgres only):
  - presence crosses nodes, and a message on the owner reaches a socket on another node;
  - `halt()` the owner, then wait for the membership change;
  - presence leave, then posting on the new owner;
  - the reconnecting client replays from a third node;
  - each message is stored once, and jobs drained by two nodes run exactly once.

Plus a real-browser smoke with two contexts: avatars both ways, a live message rendered as literal text, the sender's page not navigating, and presence updating when one leaves through a boosted link. When you change the client, also check a server restart by hand: the page reconnects and fills in what it missed.

## Gotchas agents get wrong

- **Checking `Origin` only when present.** A missing `Origin` must be refused too. Browsers always send it on WebSocket handshakes.
- **Broadcasting before writing.** If the insert fails, everyone already saw a message that doesn't exist. Write, then broadcast.
- **Replaying from the actor's memory on every node.** Only the owner has it. Elsewhere use `joined_elsewhere` (database).
- **Sending replays through `NOTIFY`.** 8,000-byte cap. Replies over the cluster must stay small.
- **Using `std::collections::hash_map::DefaultHasher` for ownership.** It's seeded per process. Use the fixed FNV-1a in `cluster.rs`.
- **`#[derive(Clone)]` on `Hub<R>`.** That requires `R: Clone`. Implement `Clone` by hand.
- **`async fn` in a trait used with `tokio::spawn`.** Declare `-> impl Future<Output = …> + Send` in the trait. `async fn` in the impl is fine.
- **Timing assertions.** `elapsed() < 150ms` passed alone and failed under parallel load. Prove a fast path by making the slow path impossible (`start_with(60s)`), not by racing a clock.
- **A dying node announcing its leaves.** A crash says nothing; the others learn it from heartbeats. `halt()` publishes nothing, so tests exercise the real path.
- **Raw `PgListener::recv()` inside a `select!` with other branches.** If another branch wins, the `recv` future is dropped mid-read. Give the listener its own task and forward over a channel ([realtime.md](realtime.md)).
- **No `hx-boost="false"`** on the socket's form (see the JS client section).
