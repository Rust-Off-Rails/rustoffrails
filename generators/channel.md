# Generator: `channel` (live updates, rooms, presence)

```
channel book:{id} reviews-changed            # signal: "viewers see new reviews appear without refreshing"
channel book:{id} --room --presence          # room: "readers chat about the book and see who's reading"
```

**Pick the kind first** ([channels.md#when-to-use-which](../guides/channels.md#when-to-use-which)):
- the page only needs to **know something changed**, so it re-fetches → a **signal** (this recipe, below; [realtime.md](../guides/realtime.md));
- the client **sends** events (chat, typing, moves), or the page shows **who's here** → a **room** ([the room recipe](#room-recipe---room)).

Both were executed in the reference app (htmx) on 2026-10-12.

## Signal recipe

1. **Spec:** add an acceptance criterion such as "Readers looking at a book see new reviews appear without refreshing" (`AC-reviews-<n>`) to the feature spec.
2. **First channel only:**
   - `sea-orm-cli migrate generate create_broadcasts` (`id`, `stream`, `event`, `created_at`), then migrate and prove rollback;
   - `src/kernel/pubsub.rs` per the spec;
   - `pubsub: PubSub::new(db.clone())` in `AppState::new`;
   - in `main.rs` `serve`: `state.pubsub.start().await?` before building the router, and `pubsub.close()` inside the graceful-shutdown future;
   - `pubsub::delete_older_than(db, 1 day)` in `CleanupJob`;
   - `src/channels/mod.rs` with `stream` + `authorize_stream`, the route `/streams/{stream}`, `paths::stream(name)`, and a `ROUTES` row with `Access::User`.
3. **This channel:**
   - `src/channels/<plural>.rs` with `authorize(s, user, id)`: load the record, then `policies::authorize(policies::<plural>::can_view(..))`;
   - a new arm in `authorize_stream` (`Some(("book", id)) => books::authorize(..)`);
   - the stream-name function in the model (`review::stream(book_id)`) and the event const (`review::CHANGED = "reviews-changed"`).
4. **Broadcast from the model fns** that change what viewers see (create, update, destroy): `pubsub::broadcast(&txn, &stream(book_id), CHANGED)`, inside the transaction, after the write succeeded.
5. **Live region, by mode:**
   - `htmx`:
     - a partial (`templates/<plural>/_list.html`) whose root carries `hx-get`, `hx-trigger="sse:<event>, sse:refresh"` and `hx-swap="outerHTML"`, included inside `<div hx-ext="sse" sse-connect="…">`;
     - a GET route that returns the fragment for `HxRequest` (redirect otherwise), e.g. `GET /books/{book_id}/reviews` → `reviews::index`;
     - the sse extension script via `{% block head %}`.
   - `inertia`: a `useEventStream(path, { "<event>": () => router.reload({ only: ["<prop>"] }), refresh: () => router.reload() })` hook in `web/src/lib/`.
   - `spa`: a `useStream` hook calling `queryClient.invalidateQueries(…)`.
6. **Tests** (`tests/app/<plural>_test.rs` and `kernel_pubsub_test.rs` for the first channel): the cases in [realtime.md#testing](../guides/realtime.md#testing). Use `open_stream`, `poll_once` and `next_event`, never sleeps.
7. **Real-browser smoke:** two contexts. One watches the page, the other makes the change, and the watcher sees it without a reload. Repeat after a Back/Forward on the watcher ([test-standards §9](../guides/test-standards.md#9-real-browser-smoke)).

## Room recipe (`--room`)

Implements [guides/channels.md](../guides/channels.md).

1. **Spec first:** `product/features/<topic>_room.md` with AC IDs for:
   - presence: join and leave, two tabs counting as one;
   - delivery and storage;
   - validation reasons;
   - replay after a reconnect;
   - who may join (logged out, private topics);
   - the per-person rate limit;
   - text shown as typed.

   Ask the feature-level questions first: who can join, is anything stored, who can delete.
2. **First room only (the kernel):**
   - `cargo add axum --features ws` and `cargo add --dev tokio-tungstenite@<the version axum resolved>`. Check `Cargo.lock` so the test client doesn't compile a second tungstenite;
   - `src/kernel/hub.rs`, `src/kernel/channels.rs` and `src/kernel/cluster.rs` per the spec, plus the `cluster_nodes` migration. That migration is cheap and keeps step 3 below free later;
   - `hub: Hub::new(config.node.clone().unwrap_or_else(node_id))` in `AppState`;
   - `APP_NODE` and `APP_CLUSTER` in `Config` and `.env.example`;
   - in `serve`: `kernel::cluster::start(..)` when `config.cluster`, and `hub.close()` in the graceful-shutdown future;
   - `src/channels/mod.rs`: the `socket` handler, `member_for`, the `AppRoom` enum with `impl Room`, and `impl Channels for AppState`;
   - the `/socket` route, `paths::socket()`, and a `ROUTES` row with `Access::User`;
   - `static/channels.js` (htmx), loaded in the layout before `app.js`, or `web/src/lib/socket.ts` (TS modes);
   - `/dev/sockets` with its tab;
   - `Ws`, `serve` and `session_cookie` in `tests/app/support.rs`.
3. **This room:**
   - **data:** a migration and model for anything durable (messages), with a `(parent_id, id)` index for replay;
   - **room code** in `src/channels/<topic>_room.rs`:
     - state (the newest N items in a `VecDeque`, and a `complete` flag);
     - `load`, `joined` (from memory when it covers `last_seen`), `joined_from_db`;
     - `handle_in`, in this order: rate limit → validate → **insert** → memory → `out.broadcast` → reply;
   - **wiring:**
     - an `AppRoom` variant and its arms;
     - an arm in `authorize` that reuses the page's policy;
   - **the page:**
     - the server renders the newest N items with `data-id`;
     - the client joins with `last_seen = max(data-id)`;
     - put `hx-boost="false"` on the room's form.
4. **Tests:** every case in [channels.md#testing](../guides/channels.md#testing) for this room. If the app runs on Postgres in production, the cluster case still applies to the kernel.
5. **Real-browser smoke:**
   - two contexts: avatars both ways, a live message rendered as literal text, and leaving through a boosted link;
   - then once by hand: restart the server with both pages open, and check they reconnect and fill in what they missed.
