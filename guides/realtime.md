# Realtime (Action Cable / Turbo Streams → signals over SSE)

**Broadcast signals, not HTML.** When data changes, the server announces "book 42's reviews changed" on a stream. Clients that care re-fetch through the normal, already-authorized GET routes. This is Turbo 8's "page refresh" broadcast model. Payloads stay tiny, nothing gets rendered per subscriber, authorization can't leak through a broadcast, and it works identically in all three frontend modes.

> Verified on 2026-10-12 in the reference app (htmx, live reviews on a book page): axum 0.8.9 `Sse`, tokio-stream 0.1.19 `BroadcastStream`, htmx 2.0.11 with htmx-ext-sse 2.2.4, tests on SQLite and Postgres, and a two-browser smoke (an edit in one context appeared in the other without a refresh).

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
- **`refresh`** is reserved. The server sends it when a subscriber lagged and may have missed signals, and the client re-fetches everything it shows.

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
pub struct Signal { pub stream: String, pub event: String }

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

## The stream endpoint (`src/channels/mod.rs`)

All SSE lives under **`/streams/`**: one route, `.route("/streams/{stream}", get(channels::stream))`, with an `Access::User` row in `ROUTES`. It dispatches to a per-resource authorizer by the stream's prefix:

```rust
pub async fn stream(
    State(s): State<AppState>,
    RequireUser(user): RequireUser,
    Path(stream): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    authorize_stream(&s, &user, &stream).await?;
    let events = BroadcastStream::new(s.pubsub.subscribe())
        .filter_map(move |msg| {
            let stream = stream.clone();
            async move {
                // `data` must be non-empty: browsers silently drop events without data.
                match msg {
                    Ok(sig) if sig.stream == stream => Some(Ok(Event::default().event(sig.event).data(stream))),
                    Ok(_) => None,
                    Err(_lagged) => Some(Ok(Event::default().event("refresh").data(stream))),
                }
            }
        })
        .take_until(s.pubsub.closed());                 // ends on shutdown
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
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
- `src/channels/<plural>.rs` holds the authorizer: load the record (404 if missing), then `policies::authorize(policies::<plural>::can_view(user, &record))`.
- `filter_map` and `take_until` come from `futures::StreamExt`. `BroadcastStream` comes from `tokio-stream` (feature `sync`). Both crates come with `new`.
- **Middleware:** tower-http's `TimeoutLayer` times the response **future** (until headers), not the body, so SSE streams aren't cut at 30s. `CompressionLayer`'s default predicate skips `text/event-stream`. Verify with `curl -N -b <cookie jar> localhost:3000/streams/book:1`. You should see `event: …` followed by `data: book:1`.

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

**htmx.** Load the vendored sse extension only on pages that stream: the layout has `{% block head %}{% endblock %}`, and the page adds `<script src="{{ ctx.assets.htmx_sse }}" defer></script>`.

```html
<div hx-ext="sse" sse-connect="{{ self.stream_path() }}">     {# "/streams/book:42" #}
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
- htmx's history cache must be off (`historyCacheSize: 0`, `refreshOnHistoryMiss: true` in the layout's `htmx-config`, [frontend-htmx](frontend-htmx.md)). Otherwise a page reached with Back or Forward is a restored DOM snapshot whose `EventSource` is dead, and live updates silently stop. The real-browser smoke caught this too.

**inertia:**

```ts
useEffect(() => {
  const es = new EventSource(`/streams/book:${book.id}`)
  const reload = () => router.reload({ only: ["reviews"] })
  es.addEventListener("reviews-changed", reload)
  es.addEventListener("refresh", () => router.reload())
  return () => es.close()
}, [book.id])
```

**spa:** the same `EventSource`, calling `queryClient.invalidateQueries({ queryKey: ["books", id, "reviews"] })`. Put it in a `useStream(stream, handlers)` hook in `web/src/api/stream.ts`.

## Gotchas agents get wrong

- `Event::default().data("")`: the event never fires in a browser. Always send non-empty data.
- Broadcasting rendered HTML. Never. Send signals only.
- Broadcasting outside the transaction (or before a unique-violation check), which announces writes that rolled back.
- Forgetting `KeepAlive`, so proxies kill idle connections after ~60s.
- No `take_until(pubsub.closed())` and no `close()` on shutdown, so deploys hang on open streams.
- Starting the poll loop in tests. Call `poll_once()`.
- Leaving `EventSource` open on unmount (React). Always `close()` in cleanup.
- htmx history snapshots (see above).
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
- logged out → redirect, and an unknown prefix, malformed id or missing record → 404;
- `seek_to_end` doesn't replay history;
- `close()` ends an open stream (the body returns `None`);
- `delete_older_than` cleans up.

Plus the real-browser smoke with two contexts ([test-standards §9](test-standards.md#9-real-browser-smoke)).
