# Generator: `channel` (live updates, rooms, presence)

> **TL;DR**
> - With the `realtime` part: `cargo offrails g channel post` writes `src/channels/post.rs` (topic `post:<id>`, `authorize` on every join and push: the post's owner when `post` is a scoped model, else only the person's own `post:<their id>`; `broadcast(&s, id, event, &data)`), its arm in `Channels::authorize`, and `tests/app/post_channel_test.rs`. Broadcast after the commit; subscribe the page with `static/channels.js`.
> - Signal (the default): `channel book:{id} reviews-changed` → a broadcast in the model fn, the stream's authorization, and a live region that re-fetches.
> - Room (`--room [--presence]`) → an `offrails.v1` channel with typed `In`/`Out` messages, join/push authorization and resume.
> - Use a room only when the client talks back or needs presence; otherwise a signal.
>
> Read the rest when adding live updates, chat or presence.

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
   - in `main.rs` `serve`: start realtime (`pubsub.start()`, plus `cluster::start` with `APP_CLUSTER`) **only once migrations are current**: if `migrations::pending` is empty, start it before building the router and fail fast on error; otherwise spawn a task that waits with the worker's `wait_for_migrations` loop and starts it then ([realtime](../guides/realtime.md#kernel-spec-srckernelpubsubrs)). Never `.await?` a table read before the listener is up: on a fresh database the app crashes with `no such table: broadcasts` instead of showing the pending-migrations page. Put `pubsub.close()` inside the graceful-shutdown future;
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
   - `src/channels/mod.rs`: the `socket`, `socket_sse`, `socket_poll_open`, `socket_poll` and `socket_send` handlers (via `SessionAndUser`), `member_for(user, session)`, the `AppRoom` enum with `impl Room` (typed `In`/`Out`), and `impl Channels for AppState` (`authorize` + `session_valid`);
   - the routes `/socket`, `/socket/sse`, `/socket/sse/{conn}`, `/socket/poll`, `/socket/poll/{conn}` (GET+POST), `paths::socket()`, and `ROUTES` rows with `Access::User`;
   - session revocation: `session::destroy`/`destroy_all_for_user` write the `member:<id>` signal; `hub.watch_revocations(..)` and `hub.log_frames(..)` in `AppState::new`; `APP_REALTIME_REVALIDATE_SECS` and `APP_REALTIME_TRANSPORTS` in `Config`;
   - `static/channels.js` (htmx), loaded in the layout before `app.js`, or `web/src/lib/socket.ts` (TS modes, typed by the rooms' exported `In`/`Out`);
   - `/dev/sockets` with its tab (transport column and frame log);
   - `Ws` (offrails.v1), `serve`, `session_cookie` and `session_for` in `tests/app/support.rs`, plus `tests/app/wire_test.rs` (conformance and security, once per app).
3. **This room:**
   - **data:** a migration and model for anything durable (messages), with a `(parent_id, id)` index for replay;
   - **room code** in `src/channels/<topic>_room.rs`:
     - state (the newest N items in a `VecDeque`, and a `complete` flag);
     - typed `In` / `Out` enums (`#[serde(tag = "event", content = "data")]`, plus `TS` in TS modes);
     - `load`, `joined` (from memory when it covers `since`), `joined_from_db`, both returning `Joined { data, replay: (seq, Out), gap }`;
     - `handle_in(msg: In)`, in this order: rate limit → validate → **insert** → memory → `out.emit(row_id, Out::…)` → reply; errors are `Rejection { code, message }`;
   - **wiring:**
     - an `AppRoom` variant and its arms;
     - an arm in `authorize` that reuses the page's policy;
   - **the page:**
     - the server renders the newest N items with `data-id`;
     - the client joins with `{ since: max(data-id) }`;
     - put `hx-boost="false"` on the room's form.
4. **Tests:** every case in [channels.md#testing](../guides/channels.md#testing) for this room. If the app runs on Postgres in production, the cluster case still applies to the kernel.
5. **Real-browser smoke:**
   - two contexts: avatars both ways, a live message rendered as literal text, and leaving through a boosted link;
   - then once by hand: restart the server with both pages open, and check they reconnect and fill in what they missed.
