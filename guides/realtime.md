# Realtime (Action Cable / Turbo Streams → signals over SSE)

> **Status: a specification, not shipped code.** Nothing in `template/app` or in the parts implements the `pubsub` module, the `/streams/{stream}` SSE route, the `?since=` cursor, the `(stream, version)` render cache or the `X-Live-Refresh` gate described below. The **`realtime` part ships [channels](channels.md)** (rooms over `offrails.v1`, presence, revocation, resume by message id) and `channels::refresh`, which tells every subscriber to re-fetch a region: that is N re-fetches per change, the thundering herd this page says is solved. Until this spec is built, treat its guarantees as design goals: if a page needs a live region many people watch, build the render-once cache first, and record in `docs/decisions.md` that you did. CHEATSHEET gotchas 7, 21 and 22 describe this spec.


> **TL;DR**
> - Broadcast **signals, not HTML**: `pubsub::broadcast(&txn, "book:42", "reviews-changed")` inside the transaction; clients re-fetch through normal authorized GETs (Turbo 8's refresh model).
> - SSE at `/streams/{stream}`; every signal has an id, pages render a `?since=` cursor, and reconnects replay what was missed (or get `refresh`). Data is never empty.
> - htmx: the sse extension in the layout; one `sse-connect` per page; never reconnect by editing the attribute; `hx-sync` on live regions; a named `ping` heartbeat with a watchdog; the history cache off; a `pageshow` guard.
> - Delivery is instant in-process on SQLite and via `NOTIFY` on Postgres, with polling as the backstop. For chat or presence, see [channels](channels.md).
> - One sender **per stream** (a signal wakes only that stream's watchers); shutdown ends every stream at once, then drains for at most `APP_SHUTDOWN_DRAIN_SECS` (5).
> - **Render once per version:** a live region many people watch is rendered once per stream version (moka `try_get_with` keyed `(stream, version)`, the version bumped where the commit is observed) and shared; re-fetches carry `X-Live-Refresh` and pass a small gate so they can't starve user requests. Without it, 5,000 watchers meant 5,000 renders per change.
>
> Read the rest when adding a live region or stream, or debugging stale pages.

**Use the part for channels.** The `realtime` [part](samples.md#the-samples) is [channels](channels.md) built, tested on SQLite and Postgres and security-reviewed: channels over `offrails.v1` with SSE and long-poll fallbacks, presence and `/dev/sockets`. It does not implement the signals/streams design on this page (status note above). When the product needs this, add the part rather than writing it from this guide: `cargo offrails add-part realtime` (dependencies come first; [workflow](../workflow.md#when-the-product-grows-a-need-add-the-part)). Keep its `// SECURITY:` markers and the tests its CHECKS.md cites. This guide stays the spec: hand-write from it only when no part fits, or the human declined the part.

**Broadcast signals, not HTML.** When data changes, the server announces "book 42's reviews changed" on a stream. Clients that care re-fetch through the normal, already-authorized GET routes. This is Turbo 8's "page refresh" broadcast model. Payloads stay tiny, nothing gets rendered per subscriber, authorization can't leak through a broadcast, and it works identically in all three frontend modes.

> Verified on 2026-10-12 in the reference app (htmx, live reviews on a book page): axum 0.8.9 `Sse`, tokio-stream 0.1.19 `BroadcastStream`, htmx 2.0.11 with htmx-ext-sse 2.2.4, tests on SQLite and Postgres, a two-browser smoke (an edit in one context appeared in the other without a refresh), and a Playwright downtime test: a review changed while the page's server was down appeared once it came back, with no reload.

| Rails | Off Rails |
|-------|-----------|
| Action Cable channel | `src/channels/<plural>.rs`: the authorizer for one kind of stream |
| Solid Cable | `broadcasts` table + `kernel/pubsub.rs`, woken by `NOTIFY` (Postgres) or in-process (SQLite) |
| `broadcasts_refreshes` / `broadcast_refresh_to` | `pubsub::broadcast(&txn, "book:42", "reviews-changed").await?` |
| `turbo_stream_from @book` | htmx: `sse-connect="/streams/book:42"`; inertia/spa: `EventSource` |
| Turbo morph refresh | htmx `hx-get` + `hx-trigger="sse:<event>"`; inertia `router.reload`; react `invalidateQueries` |
| WebSockets | Channels: rooms, presence, client→server events ([channels.md](channels.md)). Use them only when signals can't do the job |

## Names

- **Streams:** `<singular>:<id>` (`book:42`) for "things about this record", or `<plural>` (`books`) for "the list changed". Build them in one place, the model: `review::stream(book_id) -> String`.
- **Events:** `<plural>-changed` (`reviews-changed`) for "re-fetch that list". It covers create, update and delete. The client re-fetches either way, so separate verbs add listeners without adding information. Use a specific event only when clients react differently (`book-deleted` → navigate away).
- **`refresh`** is reserved. The server sends it when a subscriber lagged or reconnected with a cursor older than what's retained, and the client re-fetches everything it shows.
- **`ready`** and **`bye`** are reserved too: `ready` hands the client its resume cursor on connect; `bye` (data `revoked`) means the session ended and the client must stop reconnecting.

## Table

```
broadcasts:
  id          big_integer pk auto
  stream      string
  event       string
  created_at  timestamp_with_time_zone
```

The primary key is the only index needed. The migration (`create_broadcasts`) comes with the first channel. `CleanupJob` calls `pubsub::delete_older_than(db, 1 day)`.

## Kernel spec (`src/kernel/pubsub.rs`)

```rust
pub mod broadcast_row { /* entity for `broadcasts` */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal { pub id: i64, pub stream: String, pub event: String }   // id = the row id: the resume cursor

pub struct Missed { pub signals: Vec<Signal>, pub gap: bool }
pub const SESSION_ENDED: &str = "session-ended";   // on stream `member:<id>` (channels.md#security)

#[derive(Clone)]
pub struct PubSub {
    db: DatabaseConnection,
    // One sender per stream, created by the first subscribe, dropped once its last receiver is gone:
    // a signal wakes only that stream's subscribers. Never one process-wide channel (see below).
    topics: Arc<Mutex<HashMap<String, Topic>>>,                         // std::sync::Mutex; capacity 64 each
    // Kernel watchers that need a whole family (the hub's `member:` revocations): one sender per prefix.
    prefixes: Arc<Mutex<Vec<(String, broadcast::Sender<Signal>)>>>,     // capacity 1024 each
    last_seen: Arc<AtomicI64>,
    // Counts every signal this process has observed (all streams). Monotonic, never reset while the
    // process lives: the source of render versions (see "Render once per version").
    observed: Arc<AtomicU64>,
    closed: tokio::sync::watch::Sender<bool>,
}

struct Topic {
    tx: broadcast::Sender<Signal>,
    /// `observed` at this stream's last signal (or at the first subscribe). Bumped in `poll_once`
    /// BEFORE the signal is sent, at the point the committed row is seen.
    version: u64,
}

/// INSERT a row. Call it inside the transaction that changed the data.
pub async fn broadcast(db: &impl ConnectionTrait, stream: &str, event: &str) -> Result<(), DbErr>;
pub async fn delete_older_than(db: &impl ConnectionTrait, age: chrono::Duration) -> Result<u64, DbErr>;
/// The newest signal id (0 if none): where a new subscriber's cursor starts.
pub async fn high_water(db: &impl ConnectionTrait) -> Result<i64, DbErr>;
/// What a cursor missed on one stream: the latest signal per event name, oldest first, and
/// `gap` if signals after the cursor may have been pruned (the client must re-fetch everything).
pub async fn missed(db: &impl ConnectionTrait, stream: &str, after: i64) -> Result<Missed, DbErr>;

impl PubSub {
    pub fn new(db: DatabaseConnection) -> Self;                 // in AppState::new; doesn't poll
    pub async fn start(&self) -> Result<(), DbErr>;             // start_with(250ms)
    pub async fn start_with(&self, poll_every: Duration) -> Result<(), DbErr>;  // seek_to_end, LISTEN (Postgres), spawn the loop
    pub async fn seek_to_end(&self) -> Result<(), DbErr>;       // last_seen = MAX(id): no replay after restart
    pub async fn poll_once(&self) -> Result<usize, DbErr>;      // rows with id > last_seen, by id, limit 500; sends each to its
                                                                //   stream's sender (if any) and matching prefixes, then sweeps
    pub fn subscribe(&self, stream: &str) -> broadcast::Receiver<Signal>;         // this stream only
    pub fn subscribe_prefix(&self, prefix: &str) -> broadcast::Receiver<Signal>;  // kernel watchers only ("member:")
    pub fn topic_count(&self) -> usize;                         // live per-stream senders: tests, /dev introspection
    pub fn streams(&self) -> Vec<(String, usize)>;              // each live stream and its receiver_count(): /dev/introspect.json
    pub fn version(&self, stream: &str) -> u64;                 // the stream's entry version, else `observed`: the
                                                                //   cache key for shared renders (never 0-reset)
    pub fn closed(&self) -> impl Future<Output = ()> + Send + use<>;   // resolves on shutdown
    pub fn close(&self);                                        // stop the poller, end every stream
}
```

- **Only `serve` calls `start()`** (one poller per web process). `app worker` only inserts rows. Tests never start the loop: they call `poll_once()` themselves, which makes them deterministic.
- **`start()` reads `broadcasts`, so `serve` starts it only once migrations are current.** Pending migrations (a fresh clone, a `git pull` with the server stopped): spawn a task that waits like the worker (log once, recheck every 5s) and starts realtime when the pending-migrations page's button has run them. Otherwise start it before building the router and fail fast. Found in a real build: `serve` awaited `start()` first, so a fresh database crashed with `no such table: broadcasts` and the pending-migrations page never rendered. `bin/dev` hid it by migrating first.

  ```rust
  // serve: realtime reads app tables; on a database behind the code, start it once migrated.
  if migrations::pending(&state.db).await.is_ok_and(|p| p.is_empty()) {
      start_realtime(&state).await?;                       // pubsub.start(), then cluster::start
  } else {
      let state = state.clone();
      tokio::spawn(async move {
          if wait_for_migrations(&state, "realtime", shutdown_signal()).await
              && let Err(e) = start_realtime(&state).await
          {
              tracing::error!(error = %e, "realtime failed to start");
          }
      });
  }
  ```

  `wait_for_migrations(state, who, shutdown) -> bool` is the worker's loop, shared: it logs `pending migrations: run \`app db migrate\`; <who> waiting` once, then `migrations applied; <who> starting`, and returns `false` on shutdown.
- **Per-stream senders, not one channel per process.** With a single `broadcast::Sender` every open stream wakes for every signal, filters it, and goes back to sleep: with 5,000 watchers one signal is 5,000 task wake-ups, and a lagging receiver anywhere is caused by traffic it never wanted. `subscribe(stream)` takes the map lock, gets or creates that stream's sender, and returns `tx.subscribe()`. `poll_once` looks up `row.stream` (no sender → nobody is listening, skip), sends to it, sends to every prefix sender whose prefix `row.stream` starts with, and at the end **sweeps** under the same lock: `topics.retain(|_, tx| tx.receiver_count() > 0)` (the same for `prefixes`). `poll_once` runs at least every tick, so a stream nobody watches is forgotten within 250 ms; it costs one atomic load per live stream. Hold the `std::sync::Mutex` only to look up or clone a sender, never across an `.await`.
- **Versions are bumped where the commit is observed, never where the write happens.** In `poll_once`, for each row: `let v = observed.fetch_add(1, SeqCst) + 1;` then, under the map lock, set that stream's `topic.version = v` (if it has an entry), and only then send the signal. A stream with no entry reads `observed`, so a swept and re-created stream never goes back to an old version. **Never bump at write time** (in `broadcast`, before commit): a render that runs between the bump and the commit reads the old rows and caches them under the new version, and nothing ever replaces it. Never use the row id as the version: on Postgres a lower id can commit after a higher one.
- **Prefix subscriptions are for kernel watchers, not pages.** The hub's revocation watcher is the only caller: `hub.watch_revocations(pubsub.subscribe_prefix("member:"))`. A page stream always subscribes to its exact stream name.
- `closed()` needs Rust 2024's precise capture `+ use<>`. Without it the returned future borrows `&self`, and the SSE stream can't outlive the handler (E0515).
- **Graceful shutdown ends streams at once, then drains for a bounded time.** An SSE stream never ends on its own and the server waits for open connections, so shutdown has two parts:
  1. **Stop every stream first.** The graceful-shutdown future is `async move { shutdown_signal().await; pubsub.close(); hub.close(); }`. Every SSE stream ends on `pubsub.closed()` (`take_until`) with **no** `bye`: `bye` means "stop reconnecting" (revoked), and after a restart the browser must reconnect and resume with `Last-Event-ID`. Sockets get `bye` `restart` and close 1012 ([channels](channels.md#the-connection-kernelchannelsrs)). Nothing on the way out awaits the database or a session check.
  2. **Bound the drain.** `APP_SHUTDOWN_DRAIN_SECS` (default **5**, below the platform's kill timeout and `APP_JOBS_SHUTDOWN_GRACE`): after the signal, connections that haven't closed by then are dropped. axum 0.8's `with_graceful_shutdown` has no timeout, so race it; axum-server's `Handle` takes one:

  ```rust
  // kernel::server::run: `stop` resolves on the signal (main: shutdown_signal()), then ends every stream.
  let drain = state.config.shutdown_drain;                        // APP_SHUTDOWN_DRAIN_SECS, default 5
  let (stopping_tx, mut stopping) = tokio::sync::watch::channel(false);
  let (pubsub, hub) = (state.pubsub.clone(), state.hub.clone());
  let stop = async move { stop.await; pubsub.close(); hub.close(); let _ = stopping_tx.send(true); };
  // plain HTTP (axum 0.8): race the graceful future against the drain clock; dropping it closes what's left.
  let drained = async move { let _ = stopping.wait_for(|s| *s).await; tokio::time::sleep(drain).await };
  tokio::select! {
      r = axum::serve(listener, service).with_graceful_shutdown(stop).into_future() => r?,
      () = drained => tracing::warn!(?drain, "shutdown drain timed out; dropping open connections"),
  }
  // TLS (axum-server 0.8): the handle force-closes after the duration.
  tokio::spawn(async move { stop.await; graceful.graceful_shutdown(Some(drain)); });
  ```

  Measured by a peer on 0.6.0 (5,000 SSE watchers, TLS): shutdown took **10–35 s** (the TLS branch's drain bound was 30 s; the plain-HTTP branch had none). With every stream ending on `closed()` and a 5 s bound it is under the bound by construction. Put the serve loop in one function both `main` and the tests call (`kernel::server::run(state, listener, stop: impl Future<Output = ()> + Send + 'static) -> anyhow::Result<()>`, both branches), so the shutdown test runs the real code. Apps without realtime keep the bound and drop the two `close()` calls.
- Lagging receivers get `RecvError::Lagged`. The stream turns that into a `refresh` event.
- **Wake-ups, not polling.** A committed signal is read within milliseconds. Measured: 238 ms → 0.24 ms on SQLite and → 2.1 ms on Postgres ([channels.md](channels.md#measured-reference-app-m-series-laptop-under-heavy-parallel-load-2026-10-12)).
  - **Postgres:** `broadcast` also runs `SELECT pg_notify('offrails_pubsub', '')` on the caller's connection. NOTIFY is transactional: it's delivered at commit, and dropped on rollback. A dedicated task owns a `sqlx::postgres::PgListener` (via `sea_orm::sqlx` and `db.get_postgres_connection_pool()`, so no new crate) and forwards each notification over an `mpsc` channel. Don't call `recv()` directly inside the poller's `select!`, where other branches can cancel it mid-read.
  - **SQLite:** `broadcast` calls `WAKE.notify_waiters()` on a `static WAKE: Notify`. The insert runs inside the writer's transaction, before its commit, so after a wake-up the poller retries at 0, 2, 10 and 50 ms until the row is visible.
  - **The 250ms tick** stays as a backstop, and it's how a separate `app worker` process on SQLite reaches the web process.
  - **Tests** prove the wake-up path with `start_with(Duration::from_secs(60))`: a delivery within seconds can't have come from the poll. Never assert on elapsed time; it flakes under parallel load.

## Resume

A signal sent while a tab is disconnected (a deploy or restart, a network blip, the server briefly unhealthy) must not be lost: otherwise the live region silently goes stale. Every signal carries its row id as the SSE `id`, and **every connection has a cursor**:

1. **The page renders its cursor.** The handler reads `pubsub::high_water(&db)` **before** the data it renders, and the live region connects with `sse-connect="/streams/book:42?since=<cursor>"`. A change between rendering and connecting has a higher id, so it's replayed.
2. **On connect with a cursor** (`?since=` or `Last-Event-ID`, whichever is newer), the server first sends what was missed: the latest signal per event name after the cursor (they're signals, so one of each is enough), or `refresh` if the cursor is older than what's retained (`CleanupJob` pruned it). Then `ready` (the current cursor), then live signals.
3. **The client keeps its cursor current** from every event's id and from `ready`, and sends it on every reconnect. Native `EventSource` reconnects send `Last-Event-ID` by themselves; **htmx-ext-sse's own reconnects create a new `EventSource`, which forgets it**, so `static/app.js` overrides `htmx.createEventSource` to put the newest cursor in `?since=` (below).

The server **subscribes before reading the database**, so nothing committed in between is lost, and skips only exact duplicates of replayed ids. Never filter live signals by "id ≤ cursor": on Postgres a transaction can take a lower id and commit later.

## The stream endpoint (`src/channels/mod.rs`)

All SSE lives under **`/streams/`**: one route, `.route("/streams/{stream}", get(channels::stream))`, with an `Access::User` row in `ROUTES`. It dispatches to a per-resource authorizer by the stream's prefix:

```rust
#[derive(serde::Deserialize, Default)]
pub struct StreamQuery { pub since: Option<i64> }

pub async fn stream(
    State(s): State<AppState>,
    SessionAndUser(found): SessionAndUser,          // the session too: logout and expiry end the stream
    Path(stream): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let cfg = s.socket_config();
    if let Some(refused) = kchannels::check_fetch_site(&cfg, &headers) { return Ok(refused); }   // cross-site → 403
    let Some((sess, user)) = found else { return Err(AppError::Unauthorized) };
    let member = member_for(&user, Some(sess.id));
    if s.hub.conns_of(&member.key) >= cfg.max_conns_per_user { return Err(AppError::TooManyRequests) }
    authorize_stream(&s, &user, &stream).await?;
    let guard = kchannels::ConnGuard::register(&s, &member, "sse");     // shows in /dev/sockets, counts in caps
    let cursor = last_event_id(&headers).max(q.since);
    let rx = s.pubsub.subscribe(&stream);                                // this stream only, before reading the database
    // first: missed signals (with ids) or `refresh`, then `ready` with the cursor
    // live: signals for this stream with `.id(sig.id)`, skipping exact replayed ids; Lagged → `refresh`
    // `bye` (data "revoked") when kchannels::session_ended(..) resolves, then the stream ends
    // shutdown: pubsub.closed() ends it with no `bye`, so the browser reconnects and resumes
    // heartbeat: merge a named `ping` event (data: unix seconds) every 25s, so pages can tell idle from dead
    //   (KeepAlive comments are invisible to page JS); see "Robust live regions"
    Ok(Sse::new(first.chain(live.take_until(ended)).chain(bye).take_until(s.pubsub.closed()))
        .keep_alive(KeepAlive::default()).into_response())
}

/// Every stream is authorized; unknown prefixes and malformed ids are 404.
pub async fn authorize_stream(s: &AppState, user: &User, stream: &str) -> Result<(), AppError> {
    match stream.split_once(':') {
        Some(("book", id)) => books::authorize(s, user, id.parse().map_err(|_| AppError::NotFound)?).await,
        _ => Err(AppError::NotFound),
    }
}
```

- **Logged out → 401, never a redirect.** `shape_errors` treats `/streams/` and `Accept: text/event-stream` as the `Api` flavor ([errors-and-logging](errors-and-logging.md)): an `EventSource` that gets a 303 to the login page can't follow it, fails and retries forever, and the stream path would be saved as `return_to`. The page's own navigation is what sends a logged-out user to log in. Test: a logged-out `GET /streams/<x>` is 401 with no `Location` and no `_return_to` cookie.
- **Never send `data("")`.** Per the HTML spec, an event whose data buffer is empty is not dispatched, and `EventSource` drops it without any error. The reference app shipped this bug with every HTTP test green; only the real-browser smoke caught it. Send the stream name as data.
- **Security** ([channels.md#security](channels.md#security)): a foreign `Origin` or `Sec-Fetch-Site: cross-site` is refused; logout, a password change or revocation ends the stream at once with `bye` (the session-ended signal reaches every process); expiry ends it at the next revalidation (`APP_REALTIME_REVALIDATE_SECS`).
- `src/channels/<plural>.rs` holds the authorizer: load the record (404 if missing), then `policies::authorize(policies::<plural>::can_view(user, &record))`.
- `filter_map` and `take_until` come from `futures::StreamExt`. `BroadcastStream` comes from `tokio-stream` (feature `sync`). Both crates come with `new`.
- **Middleware:** tower-http's `TimeoutLayer` times the response **future** (until headers), not the body, so SSE streams aren't cut at 30s. `CompressionLayer`'s default predicate skips `text/event-stream`. Verify with `curl -N -b <cookie jar> localhost:3000/streams/book:1`. You should see `event: ready` with an `id:`, then `event: …` lines followed by `data: book:1`.

## Broadcasting (in model fns)

Broadcast **inside the transaction** that changed the data, so a rolled-back change never announces itself:

```rust
pub async fn create(db: &DatabaseConnection, book_id: i64, author_id: i64, new: NewReview) -> Result<Model, AppError> {
    let txn = kernel::db::begin(db).await?;
    let review = ActiveModel { /* … */ }.insert(&txn).await?;   // a unique violation returns early: no broadcast
    pubsub::broadcast(&txn, &stream(book_id), CHANGED).await?;
    txn.commit().await?;
    Ok(review)
}
```

## Render once per version (the thundering herd)

**The failure (measured by a peer on a Kanban, release build, 5,000 SSE watchers on one board, one M2):** a signal makes every watcher re-fetch its live region at once. N watchers means **N full renders of the same fragment per change**. One card move cost about 5,000 renders against a 10-connection database pool: everyone's page loads took 12–25 s, and the request timeout fired. That's the **thundering herd**: the signal-then-re-fetch model is cheap per change only if the re-fetch is cheap.

**The recipe, for every live region that more than a handful of people watch:** render the fragment **once per stream version** and serve that render to every authorized viewer.

1. **The version comes from the pub/sub kernel:** `s.pubsub.version(&stream)`, bumped in `poll_once` before the signal goes out ([kernel spec](#kernel-spec-srckernelpubsubrs)). Read it **before** rendering.
2. **The rendered HTML is cached in moka, keyed `(stream, version)`, with `try_get_with`:** moka runs one init future per key and makes every concurrent caller for that key wait for it (single-flight), so 5,000 simultaneous re-fetches cost one render.
3. **Authorization runs on every request, before the cache.** The cache only skips rendering, never the policy check.
4. **A shared render contains nothing per viewer:** no current-user names, no "edit" only on your own items, no per-session tokens. A fragment that needs per-viewer bits either keys on a coarse viewer class as well (`(stream, version, role)`), moves the bit to the client (a `data-author-id` plus the layout's current user id), or isn't cached.
5. **Filtered or personalised views (`?q=`, `?mine=1`, sort orders) bypass the cache.** They're rare, per viewer, and the request gate below still bounds them.
6. A fragment built from several streams keys on every version it reads: `(stream_a, va, stream_b, vb)`.

```rust
// AppState: shared renders for live regions. Old versions are unreachable and age out.
pub fragments: moka::future::Cache<(String, u64), Arc<String>>,
// AppState::new:
fragments: moka::future::Cache::builder().max_capacity(1_000).time_to_idle(Duration::from_secs(600)).build(),
```

```rust
// controllers/cards.rs: the live region's GET (hx-get with X-Live-Refresh, below)
pub async fn index(
    State(s): State<AppState>,
    RequireUser(user): RequireUser,
    HxRequest(htmx): HxRequest,
    Path(board_id): Path<i64>,
    Query(f): Query<CardFilter>,
) -> Result<Response, AppError> {
    let board = board::find_for(&s.db, &user, board_id).await?;           // every request: scope + policy
    policies::authorize(policies::boards::can_view(&user, &board))?;
    if !htmx {
        return Ok(Redirect::to(&paths::board(board_id)).into_response());
    }
    if f.q.is_some() {
        return Ok(Html(render_cards(&s, &board, &f).await?.to_string()).into_response()); // filtered: never shared
    }
    let stream = board::stream(board_id);
    let version = s.pubsub.version(&stream);                                // before rendering
    let html = s
        .fragments
        .try_get_with((stream, version), render_cards(&s, &board, &f))      // one render per version
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("rendering cards: {e}")))?;
    Ok(Html(html.to_string()).into_response())
}

async fn render_cards(s: &AppState, board: &board::Model, f: &CardFilter) -> Result<Arc<String>, AppError> {
    let columns = card::columns_for_board(&s.db, board.id, f).await?;
    Ok(Arc::new(views::render(CardsFragment { board_id: board.id, columns })?))
}
```

moka 0.12's `try_get_with(key, init) -> Result<V, Arc<E>>` needs `E: Send + Sync + 'static` (true for `AppError`); an error is returned to every waiter and **not** cached, so the next request retries. Checked against moka 0.12.16.

**Why the version must be bumped at observation, not at write:** a render that starts after a write-time bump but before the commit reads the old rows and caches them under the new version; every later viewer gets the stale render until the next change. Bumping in `poll_once`, after the committed row is read and before the signal is sent, means any render keyed with the new version started after the commit was visible.

**Measured after the fix** (same peer, same 5,000 watchers): page GET **10–107 ms**, card move **10–264 ms**, **0** pool-acquire warnings. Before: page loads 12–25 s and timeouts.

### Keep re-fetches from starving user requests

Even cached, a miss (or filtered views, or per-viewer fragments) can arrive 5,000 at a time. Re-fetches must never take every database connection a user's own click needs:

- **Live regions say what they are:** `hx-headers='{"X-Live-Refresh": "1"}'` on every element whose `hx-trigger` is an `sse:` event (inertia/spa: the same header on the re-fetch). It's a fairness hint, not security: a client that omits it is just a normal request, still rate-limited.
- **A small gate in front of them:** `kernel::live::gate` (a `from_fn_with_state` middleware, innermost, after flash) takes a permit from `state.refresh_permits: Arc<tokio::sync::Semaphore>` sized `(DATABASE_POOL / 3).max(1)` for any request carrying `X-Live-Refresh`, waits at most `config.refresh_wait` (5 s), and otherwise answers **503 + `Retry-After: 1`** (`AppError::Unavailable`). The permit is held until the response is produced. User requests never touch the semaphore, so at least two thirds of the pool stays free for them.
- **Timeouts are 503, not 408** ([errors-and-logging](errors-and-logging.md#response-mapping)): the pool's `acquire_timeout` is 5 s (sqlx's default is 30 s) and maps to `AppError::Unavailable`; the request `TimeoutLayer` answers `503` too, and a `Retry-After` is added to any 503 that lacks one ([routing](routing.md#middleware)). 408 means "the client was too slow to send its request", which is never what happened.

### Testing (template)

```rust
// tests/app/cards_test.rs: render once per version (realtime.md#render-once-per-version-the-thundering-herd)
#[tokio::test]
async fn live_region_renders_once_per_version() {
    let app = TestApp::new().await;
    let (owner, board) = factories::board(app.db()).await;
    app.login_as(&owner).await;
    let path = paths::board_cards(board.id);

    let before = app.get_live(&path).await;                    // HX-Request + X-Live-Refresh
    before.assert_ok();
    let card = factories::card(app.db(), &board, "Unannounced").await;   // commits + broadcasts
    // The poller hasn't observed the commit: the version is unchanged, so the shared render is served.
    let unannounced = app.get_live(&path).await;
    assert_eq!(unannounced.body, before.body, "same version, same shared render");
    // Observed: the version moved, the next fetch renders fresh.
    assert_eq!(app.state.pubsub.poll_once().await.unwrap(), 1);
    app.get_live(&path).await.assert_contains(&card.title);
    // Filtered views never share: fresh even before a signal.
    let other = factories::card(app.db(), &board, "Filtered").await;
    app.get_live(&format!("{path}?q=Filtered")).await.assert_contains(&other.title);
}

#[tokio::test]
async fn live_refreshes_wait_at_the_gate_and_answer_503_with_retry_after() {
    let app = TestApp::new().await;
    let _held: Vec<_> = (0..app.state.refresh_permits.available_permits())
        .map(|_| app.state.refresh_permits.clone().try_acquire_owned().unwrap())
        .collect();                                            // a herd holds every permit
    let (owner, board) = factories::board(app.db()).await;
    app.login_as(&owner).await;
    let res = app.get_live(&paths::board_cards(board.id)).await; // waits 5 s, then gives up
    res.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers.get("retry-after").and_then(|v| v.to_str().ok()), Some("1"));
    app.get_htmx(&paths::board_cards(board.id)).await.assert_ok(); // a user's own request isn't gated
}
```

`support::TestApp::get_live(path)` is `get_htmx` plus `X-Live-Refresh: 1`. The gate's wait is `Config.refresh_wait` (5 s; `Config::test()` sets 100 ms, not an env var) so the second test is fast; never assert on elapsed time.

## Client side

**htmx.** Load the vendored sse extension **in the layout**, on every page (`<script src="{{ ctx.assets.htmx_sse }}" defer></script>` next to htmx). Never in a page's own `{% block head %}`: a boosted navigation swaps only `<body>`, so the new page's head scripts never run and its live regions silently stay dead. The Playwright downtime test caught this; the old smoke test had passed only because it pressed Back/Forward first, which forces a full load.

```html
<div hx-ext="sse" sse-connect="{{ self.stream_path() }}">     {# "/streams/book:42?since=<cursor rendered with the page>" #}
  {% let book_id = row.book.id.clone() %}
  {% include "reviews/_list.html" %}
</div>
```

```html
{# reviews/_list.html: the live region. Its own hx-* attributes come back with every refresh. #}
<section id="reviews"
         hx-get="{{ crate::routes::paths::book_reviews(*book_id) }}"
         hx-trigger="sse:reviews-changed, sse:refresh"
         hx-headers='{"X-Live-Refresh": "1"}'
         hx-sync="this:replace"
         hx-swap="outerHTML">
  …
</section>
```

- The region's GET (`GET /books/{id}/reviews`) returns **only the fragment** (with its `<section>` wrapper) when `HxRequest` is true, and redirects to the parent page otherwise. It authorizes like the parent page. On any page more than a few people watch, it serves the shared render ([render once per version](#render-once-per-version-the-thundering-herd)).
- **After the user's own mutation, never `location.reload()`**: move the DOM optimistically, then make the same in-place GET a signal makes ([frontend-htmx](frontend-htmx.md#no-full-reload-after-a-mutation)).
- The trigger element must sit **inside** the `sse-connect` element.
- **Carry the cursor across htmx-ext-sse reconnects** (`static/app.js`): remember the newest id per stream path from `htmx:sseMessage` events and the `ready` event, and override the factory so every new `EventSource` gets `?since=` = max(rendered cursor, newest seen). Also close the source on `bye`:

```js
window.htmx.createEventSource = (url) => {
  const u = new URL(url, location.href);
  const seen = cursors.get(u.pathname);
  if (seen !== undefined && seen > Number(u.searchParams.get("since") ?? -1)) u.searchParams.set("since", String(seen));
  const source = new EventSource(u.toString());
  source.addEventListener("ready", (e) => remember(u.toString(), e.lastEventId));
  source.addEventListener("bye", () => source.close());          // session ended: stop reconnecting
  return source;
};
document.addEventListener("htmx:sseMessage", (e) => e.detail?.target?.url && remember(e.detail.target.url, e.detail.lastEventId));
```
- htmx's history cache must be off (`historyCacheSize: 0`, `refreshOnHistoryMiss: true` in the layout's `htmx-config`, [frontend-htmx](frontend-htmx.md)). Otherwise a page reached with Back or Forward is a restored DOM snapshot whose `EventSource` is dead, and live updates silently stop. The real-browser smoke caught this too.

**Robust live regions (htmx).** Verified in real browsers on a Kanban board (opencode, framework 0.3.0); each rule fixes a failure that curl and request tests can't see:

- **Never reconnect by editing `sse-connect`** (removing it, re-adding it, or changing its URL). htmx has no `MutationObserver`: the old `EventSource` closes only on `htmx:beforeCleanupElement`, which fires only for htmx-managed swaps, so every "reconnect" **leaks one open stream**. To force a reconnect, swap the whole `sse-connect` element through htmx (e.g. refresh its parent with `hx-get`), or reload the page.
- **The connection budget** (see [Many tabs stall](#many-tabs-stall-http11-vs-http2): serve HTTPS so the browser gets h2). Over HTTP/1.1 (plain-HTTP dev, or any proxy that downgrades), a browser allows **about 6 open connections per origin**, across all tabs. Each `EventSource` holds one forever. Leaked or duplicated streams, or 6 tabs of the same app, exhaust the budget, and new streams then **queue silently forever**: no error ever fires. Rules: **one `sse-connect` per page** (multiplex several signal streams through one connection by listening to several event names on it); never open streams in hidden, offscreen or duplicated elements; production behind HTTP/2 (Cloudflare, Fly, kamal-proxy with TLS) multiplexes and avoids the cap, but don't rely on it in dev.
- **Serialize refetches:** put `hx-sync="this:replace"` on every live region (or `this:abort`), so two signals in quick succession can't land out of order. Without it, two concurrent refetches race and the slower (older) response wins.
- **A named heartbeat the page can see.** SSE `KeepAlive` comments are invisible to page JS, so a client watchdog can't tell "idle" from "dead" (one agent's watchdog reloaded healthy idle boards every ~50s). Have the stream also send a named `ping` event every 25s, observe it with a hidden element (`<span hidden sse-swap="ping" hx-swap="none"></span>`; `hx-swap="none"` is a real no-op in htmx 2.0.11), and treat **no `ping` for 60s** as dead → reload the region or the page.
- **Show the connection state.** A small badge driven by `htmx:sseOpen` (live) and `htmx:sseError` (reconnecting…), so a dead stream is visible instead of a silently stale page.
- **The app layout needs the `pageshow` guard too** (`if (e.persisted) location.reload()`, the same snippet as the error pages in [errors-and-logging](errors-and-logging.md)): a back/forward-cache restore otherwise resurrects a page whose streams are dead.

**inertia** (pass the cursor as a page prop, `signal_cursor`, read before the data):

```ts
useEffect(() => {
  // Native EventSource reconnects resend Last-Event-ID by themselves; a manual reopen must pass the
  // cursor. On any re-open after the first, the server replays (or sends `refresh`), so no extra fetch.
  const es = new EventSource(`/streams/book:${book.id}?since=${signal_cursor}`)
  const reload = () => router.reload({ only: ["reviews"] })
  es.addEventListener("reviews-changed", reload)
  es.addEventListener("refresh", () => router.reload())
  es.addEventListener("bye", () => es.close())
  return () => es.close()
}, [book.id, signal_cursor])
```

**spa:** the same `EventSource` (cursor from the API response), calling `queryClient.invalidateQueries({ queryKey: ["books", id, "reviews"] })`. Put it in a `useStream(stream, cursor, handlers)` hook in `web/src/api/stream.ts`. If you ever rebuild the `EventSource` yourself (e.g. after `CLOSED`), track `e.lastEventId` and pass it as `since`.

## Many tabs stall (HTTP/1.1 vs HTTP/2)

**The failure:** with a handful of tabs open on a live page, the extra tabs sit on "connecting…" forever, and even their normal requests hang. No error is ever logged.

**The cause:** over **HTTP/1.1** a browser allows **about 6 connections per origin, across all tabs of one profile**, and every open live page holds one for its stream (SSE, or the SSE/long-poll channel transports). **HTTP/2** multiplexes every tab over one connection. **Browsers only speak HTTP/2 over TLS**, so axum's `http2` feature on a plain-HTTP listener changes nothing for a browser (h2c is only for non-browser clients).

**Measured** (reference bookshelf app, a book page with live reviews, 12 tabs in one Chrome profile, one review edited from tab 1, `.offrails/tools/browser-smoke/run.sh live-tabs`):

| Served as | Tabs that got the update |
|---|---|
| plain HTTP (HTTP/1.1) | **5/12** (the rest never) |
| HTTPS, h2 via ALPN | **12/12** and **40/40**, every run |

The same numbers came from a Kanban app built by another agent (6/12 vs 12/12 and 40/40).

**Where you get h2 automatically:** behind Cloudflare, Caddy, kamal-proxy with TLS, Fly, Render or Railway, the browser already speaks h2 to the proxy. **Development is where you must do it yourself, and only with a certificate the browser trusts.** On a self-signed certificate Chrome drops `Set-Cookie` even after "Proceed to localhost (unsafe)", so login silently bounces ([dev-tools](dev-tools.md#https-in-development)). `bin/dev` serves HTTPS only when `bin/dev-cert` produced a locally trusted certificate (mkcert with its CA installed); otherwise it serves plain HTTP/1.1 and says so (login works; live pages stall past ~6 tabs until mkcert is installed). `APP_TLS=off` forces http. The `serve` TLS branch is in [`new.md`](../generators/new.md) step 4.12. `bin/dev-cert` (copy exactly; verified in the reference app):

```sh
#!/bin/sh
# Local TLS certificate for `bin/dev`: LOCALLY TRUSTED or nothing.
# Why HTTPS: browsers only speak HTTP/2 over TLS, and over HTTP/1.1 they hold ~6 connections per
# origin, so live pages stall past ~6 tabs (guides/realtime.md, "Many tabs stall").
# Why trusted only: on a self-signed certificate Chrome drops Set-Cookie even after "Proceed to
# localhost (unsafe)", so login "succeeds" and bounces back to the login page.
# Exit 0: a trusted certificate (mkcert, CA installed) is in $dir. Exit 3: none possible here;
# bin/dev then serves plain HTTP. DEV_CERT_SELF_SIGNED=1 writes a self-signed one anyway (curl, CI).
set -eu
dir="${1:-storage/dev-certs}"
mkdir -p "$dir"
rm -f "$dir/cert.pem" "$dir/key.pem"
if command -v mkcert >/dev/null 2>&1; then
  out="$(mkcert -cert-file "$dir/cert.pem" -key-file "$dir/key.pem" localhost 127.0.0.1 ::1 2>&1)" || {
    echo "$out" >&2; exit 1; }
  # mkcert writes the files either way and only warns when its CA isn't trusted.
  untrusted='not installed in the system trust store'
  [ "$(uname -s)" = Linux ] && untrusted='not installed in the'  # Chrome on Linux reads the NSS store
  case "$out" in
    *"$untrusted"*)
      rm -f "$dir/cert.pem" "$dir/key.pem"
      echo "bin/dev-cert: mkcert's CA is not trusted by this machine yet. Run once: mkcert -install" >&2
      exit 3 ;;
  esac
  echo "bin/dev-cert: trusted certificate (mkcert) in $dir"
  exit 0
fi
if [ "${DEV_CERT_SELF_SIGNED:-}" = 1 ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -days 825 -keyout "$dir/key.pem" -out "$dir/cert.pem" \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1" >/dev/null 2>&1
  echo "bin/dev-cert: SELF-SIGNED certificate in $dir (DEV_CERT_SELF_SIGNED=1). Browsers drop cookies on it: login won't stick. For curl -k and scripted checks only." >&2
  exit 0
fi
echo "bin/dev-cert: mkcert not found; refusing to make a self-signed certificate (Chrome drops login cookies on it)." >&2
echo "  Install mkcert (macOS: brew install mkcert · Debian/Ubuntu: apt install mkcert libnss3-tools · https://github.com/FiloSottile/mkcert), then: mkcert -install" >&2
exit 3
```

`bin/dev` runs it on every start and picks the scheme from its exit code; the full script is in [dev-tools](dev-tools.md#the-bindev-script).

Prove it: with mkcert installed, `curl -s -o /dev/null -w '%{http_version}' https://localhost:$PORT/up` prints `2` **without `-k`** where curl uses the system trust store (macOS `/usr/bin/curl`; elsewhere add `--cacert "$(mkcert -CAROOT)/rootCA.pem"`), and the login smoke passes ([dev-tools](dev-tools.md#browser-smoke)).

**Size the per-user connection cap for tabs.** Each tab can hold two realtime connections (a signal stream and a channel socket). The reference app's cap of **8** per user returned **429 on `/streams/…` from about the fifth tab**, the same symptom as the HTTP/1.1 pool. The cap is **128** now: it still bounds abuse, and a real person's tabs fit. Any cap must be tested with the multi-tab smoke at ≥ 40 tabs.

**Client rule (if you ever suspend streams in hidden tabs):** only on HTTP/1.1, never on h2/h3, where streams are cheap and suspending only loses updates. A tab that **loads** hidden never gets a `visibilitychange`, so close its stream on open:

```js
// static/app.js: protocol-aware stream suspension (only needed if you suspend at all)
const multiplexed = () => /^h[23]/.test(performance.getEntriesByType("navigation")[0]?.nextHopProtocol || "");
document.addEventListener("htmx:sseOpen", (e) => {
  if (document.visibilityState !== "visible" && !multiplexed()) e.detail.source.close();
});
```

**The test that catches it** is the multi-tab smoke ([dev-tools](dev-tools.md#browser-smoke)): N tabs in one profile on a live page, one change, every tab must update. Request tests can't see the browser's connection pool.

**`spa` mode in development** browses Vite on :5173, and Vite serves HTTP/1.1 whenever `server.proxy` is set (its docs: `server.https` "downgrades to TLS only when the server.proxy option is also used"). So `spa` dev keeps the ~6-connection budget: `bin/dev` there serves the Rust app over plain http behind the proxy, and the multi-tab check runs against the built app (`pnpm --dir web build`, then `bin/dev`'s Rust server alone over HTTPS on :3000). Not yet measured in a reference `spa` app.

## Measured capacity

What peers **reached** on one box. These are observations, not guarantees: your rows, renders and hardware decide.

| What | Reached | Setup |
|---|---|---|
| Open SSE watchers held | **5,000** | 0.6.0, one process, signal streams over SSE |
| Memory, whole process | **154 MB** | the same |
| Fan-out, commit → watchers notified | p50 **≈ 40 ms** | the same |
| Page GET while 5,000 watch one board | 12–25 s and timeouts → **10–107 ms** | Kanban, release build, one M2, before → after [render once per version](#render-once-per-version-the-thundering-herd) |
| Card move (POST → done) under the same load | **10–264 ms**, 0 pool-acquire warnings | after the fix |
| Graceful shutdown with 5,000 streams | **10–35 s** on 0.6.0 | now bounded by `APP_SHUTDOWN_DRAIN_SECS` (5) |

**Open streams are cheap; the work they trigger isn't.** The bottleneck was the database and rendering, not holding streams: without the shared render, every signal cost one full render per watcher. The fixes from these runs: per-stream senders (a signal wakes only its stream's watchers), render once per version, the re-fetch gate, coalesced presence ([channels](channels.md#presence-over-sse-no-websocket)), and the bounded shutdown. In-process fan-out numbers are in [channels](channels.md#measured-reference-app-m-series-laptop-under-heavy-parallel-load-2026-10-12).

**Honest unknowns:**

- On some server restarts, **10–49 of 5,000** connections failed on the client side and didn't come back by themselves. Cause unknown (client-side limits in the load generator, the listen backlog, or reconnect timing are all candidates). Not reproduced in the reference app.
- The 10–35 s shutdown is explained by the old 30 s TLS bound and no bound on plain HTTP; the new bound was not re-measured at 5,000 streams.

## Load testing (`tools/loadgen`, spec)

A load generator for live pages, to measure before raising a limit or adding caching (evidence first). Spec for a Node script run like the browser smokes (`bash .offrails/tools/loadgen/run.sh`, dependencies in `~/.cache/offrails/`, nothing added to the app). Not shipped yet; a peer's working version exists.

**Inputs:** `BASE`, `STREAM` (the exact stream to watch, e.g. `board:17`), `PAGE` (the live region's GET), `WATCHERS` (N), `MUTATIONS` (M) and `MUTATE` (a scenario function that changes data once and returns a token the region will show), `SESSIONS` (a file of cached session cookies).

**What it does:**

1. Logs in once per distinct user it needs and **caches the session cookies to a file**, reused across runs. The login rate limiter counts failures and attempts, so logging in thousands of times per run locks you out of your own test.
2. Opens N SSE connections to `STREAM` (with `?since=` from the page), each re-fetching `PAGE` on a signal the way the browser does (`HX-Request`, `X-Live-Refresh`).
3. Performs M mutations, one at a time, and records **per-event delivery latency**: mutation committed → signal received, and → the re-fetched fragment containing the token, per watcher. Reports p50/p95/p99/max and how many watchers never saw it.
4. **Asserts presence** when the page shows it: the label reaches N tabs (and the expected people) within backoff + 1 s.
5. Samples the **server** process's RSS and open file descriptors (`ps -o rss= -p $PID`, `lsof -p $PID | wc -l`) every second, and reports the client's own CPU **separately**: a saturated load generator looks exactly like a slow server.

**Lessons baked in:**

- **Pick the target explicitly** (`STREAM`, `PAGE`, the user). Never "the first board in the list": that silently measured a different, smaller board.
- **The per-user connection cap is 128** (channels and streams, per node). N connections need at least ⌈N/128⌉ users (10,000 → 79); with fewer, the excess is refused (429 on streams, `bye` `limit` + 1008 on sockets). The generator spreads connections over its users and, when any are refused or closed, **prints the cap as the reason** ("closed: 1,240, refused by the per-user cap (128 × 70 users = 8,960)") instead of a bare "closed" count that looks like a server failure.
- **One Node process tops out at about 5,000 TLS + h2 streams.** Beyond that, shard across processes (`--shard i/n`) and merge the reports.
- **Never `--ignore-certificate-errors` or `rejectUnauthorized: false` on a login check.** Trust the dev certificate (mkcert) or use the plain-HTTP port; the browser smokes run with no certificate overrides for the same reason ([dev-tools](dev-tools.md#https-in-development)).
- Report what was **reached**, with the setup (build profile, machine, N, M, database), never a capacity promise.

## Gotchas agents get wrong

- `Event::default().data("")`: the event never fires in a browser. Always send non-empty data.
- Broadcasting rendered HTML. Never. Send signals only.
- Broadcasting outside the transaction (or before a unique-violation check), which announces writes that rolled back.
- Forgetting `KeepAlive`, so proxies kill idle connections after ~60s.
- No `take_until(pubsub.closed())` and no `close()` on shutdown, so deploys hang on open streams. Or an unbounded drain: 5,000 streams took 10–35 s to shut down. End streams first, then drain for `APP_SHUTDOWN_DRAIN_SECS` (5).
- **One `broadcast` channel per process** that every stream subscribes to and filters: every signal wakes every stream. Subscribe per stream (`pubsub.subscribe(&stream)`).
- Sending `bye` on shutdown: the client stops reconnecting and the page goes stale after a deploy. `bye` is for revocation only.
- **The thundering herd:** every watcher re-fetching and re-rendering the same fragment after a signal. 5,000 watchers = 5,000 renders per change (page loads of 12–25 s). Render once per version ([recipe](#render-once-per-version-the-thundering-herd)).
- **Bumping a render version at write time** (in `broadcast`, before commit): a concurrent render caches pre-commit data under the new version for good. Bump in `poll_once`, then send.
- **Live re-fetches sharing the whole pool with user clicks:** gate `X-Live-Refresh` requests with the semaphore; timeouts answer 503 + `Retry-After`, never 408.
- Starting the poll loop in tests. Call `poll_once()`.
- Leaving `EventSource` open on unmount (React). Always `close()` in cleanup.
- htmx history snapshots (see above).
- **No cursor**: a page rendered without `?since=`, or htmx reconnects without the `createEventSource` override, loses every signal sent while it was disconnected (the reference app shipped this; the human noticed a stale review list after a deploy).
- **The sse extension in a page's head block**: dead after any boosted navigation (see Client side).
- **Plain-HTTP dev with live pages:** realtime breaks at ~6 tabs. Install mkcert (`mkcert -install`) so `bin/dev` serves HTTPS (h2); never "fix" it with a self-signed certificate, which breaks login ([Many tabs stall](#many-tabs-stall-http11-vs-http2), [dev-tools](dev-tools.md#https-in-development)).
- **A per-user connection cap sized for one tab:** 429 on streams from the fifth tab. Size it for tabs (128) and prove it with the multi-tab smoke.
- **Reconnecting by editing `sse-connect`, unserialized refetches, no visible heartbeat, more than one stream per page:** see "Robust live regions" above. Together they turn into "the board silently goes stale after a while".
- Fly/Kamal proxies: SSE works over HTTP/1.1 and HTTP/2 with no special config. WebSocket channels have their own notes in [deployment](deployment.md).

## Testing

`TestApp::open_stream(path) -> (StatusCode, Body)` makes the request and returns the unread body. `support::next_event(&mut body)` returns the next event **as a browser would dispatch it**: frames without a non-empty `data:` line are skipped, and it returns `None` after a second of silence. Never sleep.

```rust
let (status, mut stream) = app.open_stream(&format!("/streams/book:{}", b.id)).await;
assert_eq!(status, StatusCode::OK);
create_review(app.db(), &b, &poster).await;                       // commits a broadcast
assert_eq!(app.state.pubsub.poll_once().await.unwrap(), 1);
assert_eq!(next_event(&mut stream).await.as_deref(), Some("reviews-changed"));
app.get_htmx(&format!("/books/{}/reviews", b.id)).await.assert_contains(&review.body);
```

Required cases:
- a committed change is delivered to a subscriber;
- another stream's signal isn't;
- a rolled-back write delivers nothing (assert `next_event` is `None`);
- logged out → 401 (no `Location`), and an unknown prefix, malformed id or missing record → 404; cross-site → 403;
- `seek_to_end` doesn't replay history;
- **resume:** a reconnect with `Last-Event-ID` gets the missed signals (one per event name, with ids), then `ready`; the same with `?since=`; a cursor older than retention gets `refresh`; a first connection gets `ready` with the current cursor and no history (`support::next_sse` returns `(event, id)` including `ready`; `next_event` skips `ready`);
- **session:** logout / password change ends the stream with `bye`; an expired session ends it at revalidation;
- `close()` ends an open stream (the body returns `None`) without a `bye`;
- **per-stream delivery:** a subscriber of `book:1` and one of `book:2`; a signal on `book:1` reaches the first, and the second's `try_recv()` is `Err(Empty)` (it was never woken, not just filtered);
- **sweep:** `subscribe("book:1")`, drop the receiver, `poll_once()`: `topic_count()` is back to 0; a `subscribe_prefix("member:")` receiver gets `member:7` signals and not `book:1`;
- **bounded shutdown** (`server_test.rs`, real port via `kernel::server::run`): with 200 streams open, firing `stop` ends every body (EOF, no `bye`) and `run` returns well inside `timeout(drain + 5 s)` with `drain` = 30 s (prompt, not the bound); and with `drain` = 1 s and one connection whose response never ends (a test-only route), `run` still returns within `timeout(6 s)` (the bound holds). Generous timeouts, never an `elapsed() <` assertion;
- `delete_older_than` cleans up;
- **versions:** `version(s)` is unchanged by a committed but unpolled signal, and greater after `poll_once`; a stream swept and re-subscribed never returns an older version;
- **render once per version** and **the re-fetch gate**: the two templates in [Render once per version](#testing-template).

Plus the real-browser checks ([test-standards §9](test-standards.md#9-real-browser-smoke)): two contexts see each other's changes live, and the Playwright downtime test (stop the server, change data through another process, restart: the open page catches up without a reload).
