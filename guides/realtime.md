# Realtime (Action Cable / Turbo Streams → signals over SSE)

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
    tx: tokio::sync::broadcast::Sender<Signal>,      // capacity 1024
    last_seen: Arc<AtomicI64>,
    closed: tokio::sync::watch::Sender<bool>,
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
    pub async fn poll_once(&self) -> Result<usize, DbErr>;      // rows with id > last_seen, by id, limit 500
    pub fn subscribe(&self) -> broadcast::Receiver<Signal>;
    pub fn closed(&self) -> impl Future<Output = ()> + Send + use<>;   // resolves on shutdown
    pub fn close(&self);                                        // stop the poller, end every stream
}
```

- **Only `serve` calls `start()`** (one poller per web process). `app worker` only inserts rows. Tests never start the loop: they call `poll_once()` themselves, which makes them deterministic.
- `closed()` needs Rust 2024's precise capture `+ use<>`. Without it the returned future borrows `&self`, and the SSE stream can't outlive the handler (E0515).
- **Graceful shutdown:** `axum::serve(...).with_graceful_shutdown(async move { shutdown_signal().await; pubsub.close(); })`. axum waits for open connections, and an SSE stream never ends on its own, so without `close()` every deploy would hang until the platform's kill timeout. Verified: with two SSE streams open, the server exits immediately on SIGTERM.
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
    let rx = s.pubsub.subscribe();                                       // before reading the database
    // first: missed signals (with ids) or `refresh`, then `ready` with the cursor
    // live: signals for this stream with `.id(sig.id)`, skipping exact replayed ids; Lagged → `refresh`
    // `bye` (data "revoked") when kchannels::session_ended(..) resolves, then the stream ends
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
         hx-swap="outerHTML">
  …
</section>
```

- The region's GET (`GET /books/{id}/reviews`) returns **only the fragment** (with its `<section>` wrapper) when `HxRequest` is true, and redirects to the parent page otherwise. It authorizes like the parent page.
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
- **The connection budget.** Over HTTP/1.1 (local dev, or any proxy that downgrades), a browser allows **about 6 open connections per origin**, across all tabs. Each `EventSource` holds one forever. Leaked or duplicated streams, or 6 tabs of the same app, exhaust the budget, and new streams then **queue silently forever**: no error ever fires. Rules: **one `sse-connect` per page** (multiplex several signal streams through one connection by listening to several event names on it); never open streams in hidden, offscreen or duplicated elements; production behind HTTP/2 (Cloudflare, Fly, kamal-proxy with TLS) multiplexes and avoids the cap, but don't rely on it in dev.
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

## Gotchas agents get wrong

- `Event::default().data("")`: the event never fires in a browser. Always send non-empty data.
- Broadcasting rendered HTML. Never. Send signals only.
- Broadcasting outside the transaction (or before a unique-violation check), which announces writes that rolled back.
- Forgetting `KeepAlive`, so proxies kill idle connections after ~60s.
- No `take_until(pubsub.closed())` and no `close()` on shutdown, so deploys hang on open streams.
- Starting the poll loop in tests. Call `poll_once()`.
- Leaving `EventSource` open on unmount (React). Always `close()` in cleanup.
- htmx history snapshots (see above).
- **No cursor**: a page rendered without `?since=`, or htmx reconnects without the `createEventSource` override, loses every signal sent while it was disconnected (the reference app shipped this; the human noticed a stale review list after a deploy).
- **The sse extension in a page's head block**: dead after any boosted navigation (see Client side).
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
- logged out → redirect, and an unknown prefix, malformed id or missing record → 404; cross-site → 403;
- `seek_to_end` doesn't replay history;
- **resume:** a reconnect with `Last-Event-ID` gets the missed signals (one per event name, with ids), then `ready`; the same with `?since=`; a cursor older than retention gets `refresh`; a first connection gets `ready` with the current cursor and no history (`support::next_sse` returns `(event, id)` including `ready`; `next_event` skips `ready`);
- **session:** logout / password change ends the stream with `bye`; an expired session ends it at revalidation;
- `close()` ends an open stream (the body returns `None`);
- `delete_older_than` cleans up.

Plus the real-browser checks ([test-standards §9](test-standards.md#9-real-browser-smoke)): two contexts see each other's changes live, and the Playwright downtime test (stop the server, change data through another process, restart: the open page catches up without a reload).
