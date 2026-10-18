# realtime

Channels for a Rust off Rails app: one connection per tab speaking **`offrails.v1`** over a WebSocket, falling back by itself to Server-Sent Events + POST and then long polling when a proxy breaks WebSockets. Typed rooms (`Room`: `joined`, `handle_in`, `handle_out`), topics authorized on every join and every push, presence that lists people (not tabs) across tabs, devices and nodes, resume after a reconnect from a durable cursor, and a session that is checked for the connection's whole life. It's what Phoenix Channels + Presence and Action Cable give you, with the security rules written down and tested, and no Redis: more than one node talks over Postgres `LISTEN/NOTIFY`.

- **Protocol**: typed JSON frames (`join`, `leave`, `push`, `ping` up; `ok`, `err`, `event`, `presence`, `pong`, `slow`, `bye` down), refs echoed in replies, heartbeats both ways, `seq` cursors and replay after a reconnect, a `gap` when too much was missed, stable error codes and close codes.
- **Transports**: WebSocket first; SSE + POST, then long polling, picked by `static/channels.js` (no framework) when one is blocked; it remembers what worked in the tab and probes back to the WebSocket every minute. WebSocket buffers tuned for 10,000 sockets on one box (4 KiB reads, no write staging, a 256 KiB backpressure cap).
- **Who's connecting**: the session cookie with an exact `Origin` check (cross-site WebSocket hijacking), or a 60-second, single-use, HMAC-signed **socket token** (offered as a subprotocol, never in the URL) for clients on another origin or no browser at all. The person always comes from the server's session, never from a frame.
- **Authorization**: `Channels::authorize` decides every topic, **deny by default**, against the database at every join, every push, every revalidation and on `Hub::reauthorize` (someone removed from a room leaves it at once, on every node). A members-only room and a missing one give the same answer.
- **Revocation**: signing out closes that session's sockets and streams (`bye revoked`, 4401) on every node and leaves the person's other devices alone; an expired session is closed by the next revalidation (60 s).
- **Limits**: 128 connections per person (sized for tabs; checked and counted under one lock), 20 topics and 30 joins a minute per connection, 100 frames per 10 s, 4 KiB per frame, 10 messages per 10 s per person per room, slow consumers warned then dropped, a write stalled for 10 s ends the connection.
- **Presence**: CRDT-free and correct across tabs, devices and nodes: each node is the only writer of its own refs and publishes them in order; a new node asks for everyone's refs; a silent node's people leave after 6 s, a cleanly stopped one's at once. Minimal metadata (a name, initials, a color), never an email.
- **Server API**: `hub.broadcast`, `broadcast_from` (everyone but the sender), `emit` (typed), `reauthorize`, `revoke`; in a room, `out.emit` (durable), `emit_ephemeral`, `broadcast_from`, `emit_to` (another topic); `channels::refresh(topic, region)` makes every open page re-fetch an htmx region.
- **Demo**: a two-pane live room at `/live/<room>`: who's here with avatars and a wave button, messages, "is typing…", an htmx header refreshed on every page when someone edits it. Development guests sign in with a display name, so it runs on a blank app.
- **`/dev/sockets`**: this node and the cluster, every topic with who is present, every connection with its transport and frame counts, frame rates, room restarts, and a live decoded frame log per connection.

Extracted from Rust off Rails apps and rebuilt. Fixed on the way: a per-user cap of 8 that locked people out at about five tabs; a cap check and the count that followed it under two locks (concurrent connects overshot); one forwarder task and channel per joined topic (the connection task reads its receivers itself now); a logout that slipped between the handshake and the connection's start; relays bound to the person but not the session; an unknown relay id answering differently from someone else's. Added: rooms with typed pushes in the same hub as presence, SSE and long-poll fallbacks around the tuned WebSocket loop, `handle_out`, `broadcast_from`, socket tokens, cluster presence without an owner node, `/dev/sockets`, and the htmx refresh helper.

Guides: [`.offrails/guides/channels.md`](https://rustoffrails.org/docs/guides/channels) · [`.offrails/guides/realtime.md`](https://rustoffrails.org/docs/guides/realtime) · [`.offrails/guides/security.md#websockets`](https://rustoffrails.org/docs/guides/security). Depends on: nothing (it ships development guests). With [auth-core](../auth-core/) installed too, before or after, people are accounts instead: optional wiring does it ("Use your accounts" below).

## Screens

<a href="../../docs/screenshots/realtime/live-room-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/realtime/live-room-dark-desktop.png"><img src="../../docs/screenshots/realtime/live-room-light-desktop.png" alt="The live room with two people talking" width="860"></picture></a>

<table>
<tr><td valign="top"><a href="../../docs/screenshots/realtime/presence-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/realtime/presence-dark-desktop.png"><img src="../../docs/screenshots/realtime/presence-light-desktop.png" alt="Presence, typing and a wave" width="420"></picture></a><br><sub>Presence and typing</sub></td><td valign="top"><a href="../../docs/screenshots/realtime/dev-sockets-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/realtime/dev-sockets-dark-desktop.png"><img src="../../docs/screenshots/realtime/dev-sockets-light-desktop.png" alt="The /dev/sockets page: connections, topics and frames" width="420"></picture></a><br><sub>/dev/sockets</sub></td></tr>
</table>

<sub>Light or dark follows your GitHub theme; click an image for full size.</sub>

| Screen | Light | Dark |
|--------|-------|------|
| Live room, two people talking | [desktop](../../docs/screenshots/realtime/live-room-light-desktop.png) · [mobile](../../docs/screenshots/realtime/live-room-light-mobile.png) | [desktop](../../docs/screenshots/realtime/live-room-dark-desktop.png) · [mobile](../../docs/screenshots/realtime/live-room-dark-mobile.png) |
| Presence, typing and a wave | [desktop](../../docs/screenshots/realtime/presence-light-desktop.png) · [mobile](../../docs/screenshots/realtime/presence-light-mobile.png) | [desktop](../../docs/screenshots/realtime/presence-dark-desktop.png) · [mobile](../../docs/screenshots/realtime/presence-dark-mobile.png) |
| `/dev/sockets` with a frame log | [desktop](../../docs/screenshots/realtime/dev-sockets-light-desktop.png) · [mobile](../../docs/screenshots/realtime/dev-sockets-light-mobile.png) | [desktop](../../docs/screenshots/realtime/dev-sockets-dark-desktop.png) · [mobile](../../docs/screenshots/realtime/dev-sockets-dark-mobile.png) |
| Development sign-in | [desktop](../../docs/screenshots/realtime/sign-in-light-desktop.png) · [mobile](../../docs/screenshots/realtime/sign-in-light-mobile.png) | [desktop](../../docs/screenshots/realtime/sign-in-dark-desktop.png) · [mobile](../../docs/screenshots/realtime/sign-in-dark-mobile.png) |

## Install

By hand, in the app root:

1. Crates, with `cargo add` (never a version): `cargo add axum --features ws`, `cargo add hmac`, `cargo add --dev tokio-tungstenite`.
2. Copy `files/` into the app, keeping the paths. In the tests and the `factories.rs` wiring, replace `offrails_host` with your crate name.
3. Wiring (each at its `samples: <anchor>` comment when the app has one; otherwise at the spot described). The exact text of each is in `sample.toml`:
   - `src/lib.rs`: `pub mod channels;`.
   - `src/kernel/mod.rs`: `pub mod channels; pub mod cluster; pub mod hub; pub mod socket_token; pub mod sockets_web; pub mod transports;`.
   - `src/kernel/ui.rs`: a `Tab { key: "sockets", label: "Sockets", href: "/dev/sockets", dev_only: true }` in the kernel header's `TABS`.
   - `src/models/mod.rs`: `pub mod guest; pub mod guest_session; pub mod room; pub mod room_message;`. `src/controllers/mod.rs`: `pub mod live;`. `src/views/mod.rs`: `pub mod live;`.
   - `src/config.rs`: a `realtime: RealtimeConfig` field, its `Debug` line, `let realtime = RealtimeConfig::parse(&get, &mut invalid, env, &database_url);` next to the other parsing, `realtime,` in `Ok(Self { … })`, `realtime: RealtimeConfig::test(),` in `Config::test()`, and the `RealtimeConfig` struct after `impl Config`.
   - `src/state.rs`: `pub hub: crate::kernel::hub::Hub<crate::channels::AppRoom>`, built with `let hub = crate::channels::build_hub(&config, &db)?;` before `Ok(Self { … })`.
   - `src/routes.rs`: `app = app.merge(crate::channels::router()).merge(crate::controllers::live::router());` with the other routes; inside `if config.is_development() { … }`, `app = app.merge(crate::controllers::live::dev_router()).merge(crate::kernel::sockets_web::router());`; `paths::live()` and `paths::dev_sockets()`; the seventeen `ROUTES` rows.
   - `src/controllers/dev.rs`: the Sockets and Live room tiles, and two system facts.
   - `src/kernel/introspect.rs`: `out["realtime"]`.
   - `src/main.rs`, in `serve`: `state.hub.start().await?; let realtime_hub = state.hub.clone();` after the router is built, and `realtime_hub.close();` in the shutdown future, before the drain starts.
   - `migration/src/lib.rs`: the three `m20261016_0050…` migrations (`mod` lines and `Box::new(…)` entries).
   - `templates/shared/_nav.html`: a "Live room" link; `templates/layouts/application.html`: a "Sockets" link in the development footer, `hx-boost="false"` (a kernel page, in its own layout).
   - `src/views/mod.rs`: two `Ctx` fields, `channels_js` and `live_room_js` (digested URLs); `src/controllers/application.rs` fills them; `templates/layouts/application.html`: both `<script … defer>` tags at `samples: app head`, so every app page loads them. Never in the room's `{% block head %}`: links are boosted, and a boosted click runs nothing in the next page's `<head>`. `live_room.js` starts a `[data-live-room]` on `htmx:load` and leaves its topics before a boosted swap removes it.
   - `tests/app/main.rs`: `mod realtime_test;`; `tests/app/factories.rs`: `find_room`; `tests/app/security_test.rs`: `src/channels/mod.rs`, `src/controllers/live.rs` and `src/kernel/sockets_web.rs` in the route sources; `tests/app/kernel_web_test.rs`: the three `/dev/sockets` paths in the no-store list; `tests/app/theme_switch_test.rs`: `"/dev/sockets"` in `KERNEL_PAGES`.
   - `.env.example`: the six `APP_REALTIME_*`, `APP_CLUSTER` and `APP_NODE` lines.
   - Anchors of its own, for the optional wiring: `// samples: channel mods` (`src/channels/mod.rs`), `// samples: identity source` (`src/channels/identity.rs`), `// samples: realtime test mods` and `// samples: realtime people` (`tests/app/realtime_test/`). They ship in `files/`.
   - **With auth-core installed** (the `when = "auth-core"` wires; `install.sh` applies them whichever of the two comes first): `pub mod accounts;` in `src/channels/mod.rs`; `pub use crate::channels::accounts::{DEV_GUESTS, by_session, current, session_live};` above the guests' glob in `identity.rs`; `crate::channels::session_ended(state, user_id);` in auth-core's `auth::sessions_ended`; `mod accounts; mod people_accounts;` in `tests/app/realtime_test/mod.rs`, and their `pub use` in `people.rs`.
4. `cargo fmt --all && cargo run -- routes` (rewrites `docs/routes.md`), `bin/tailwindcss -i assets/app.css -o static/app.css`, then the Definition of Done.

`tools/install.sh realtime <app>` does steps 1 to 3.

## After installing

- **Accounts**: alone, the sample signs people in as development guests (`/dev/guests/sign-in`, development only), and in production nobody is signed in, so every socket answers 401. With auth-core, people are accounts already ("Use your accounts" below); with another auth system, adapt `identity.rs` the same way before shipping.
- **Deploy config**: `APP_REALTIME_SECRET` (32+ random characters) if any client uses socket tokens; `APP_CLUSTER=true` when more than one node serves sockets (Postgres only); behind a proxy that breaks WebSockets, `APP_REALTIME_TRANSPORTS=sse,longpoll`. Serve over HTTP/2 (TLS): over HTTP/1.1 a browser holds about six connections per origin, and the fallbacks hold one each ([realtime](https://rustoffrails.org/docs/guides/realtime)).
- **`docs/decisions.md`**: note that channels replay from the database (no owner node), that per-room rate limits are per node, and which transports you offer.
- **`AGENTS.md`**: nothing changes in the header; mention `/dev/sockets` and `tests/e2e/live.scenario.mjs` where you list the app's tools.
- Try it: `bin/dev`, open `/live` in a browser and in a private window, sign in as two people, talk; open `/dev/sockets` in a third tab. In a real browser: `BASE=… node tests/e2e/live_room.e2e.mjs` (two people, typing, a wave, both fallbacks, sign-out) and `SCENARIO=tests/e2e/live.scenario.mjs BASE=… TABS=40 bash .offrails/tools/browser-smoke/run.sh live-tabs`.

## Adapting it

- **Use your accounts** (auth-core): install auth-core (before or after this sample) and the optional wiring does the rest: `src/channels/accounts.rs` becomes part of the build and `identity.rs` re-exports it over the guests, so `current` reads auth-core's session cookie, `by_session` and `session_live` check auth-core's `sessions` (idle and absolute expiry included), and auth-core's `auth::sessions_ended` (called after every sign-out, sign-in over another session, revoked device, "sign out everywhere", password change and reset, "this wasn't me") closes that person's sockets at once. The live room then sends signed-out people to `/session/new`, and the guest pages redirect there. Presence shows a neutral "Member 42", never the address nor a name guessed from it (an address's local part is often a full name or worse): `accounts::display_name` (`// APP:`) is the place for your own name column, once people choose a name. The whole realtime suite runs against accounts (`tests/app/realtime_test/people_accounts.rs`), plus `accounts.rs` for sign-out and the guest pages. Once nobody uses guests, delete `models/guest.rs`, `models/guest_session.rs`, the `guests` module in `identity.rs`, the `dev_router` in `controllers/live.rs` and `people.rs`'s `guests` module; the guest migration stays (append-only) and its tables stay empty.
- **Another auth system**: write `src/channels/<yours>.rs` like `accounts.rs` (the same four items: `current`, `by_session`, `session_live`, `DEV_GUESTS`) and add `pub use crate::channels::<yours>::{DEV_GUESTS, by_session, current, session_live};` above the guests' glob in `identity.rs`. Call `crate::channels::session_ended(&state, user.id)` after every place a session ends.
- **A per-board channel** (or a document, a match): copy `src/channels/live_room.rs` to `board_room.rs` with its own `In`/`Out` enums, add a variant to `AppRoom` (the `init` dispatch by topic prefix), and an arm to `authorize` in `src/channels/mod.rs`: `Some(("board", id)) => boards::authorize(self, member, id).await`. For a board many people watch, carry the state itself (the join's `ok.data` is the snapshot plus its `seq`; patches carry final state through one reducer; actions stay HTTP POSTs that write an event row and broadcast after the commit): [channels](https://rustoffrails.org/docs/guides/channels), "Live boards over a channel".
- **Only members can join**: the `backstage` room shows it. `room::admits` checks `room_members` against the database on every join and push; after `room::remove_member`, call `s.hub.reauthorize(&room::topic(slug))` and every connection of that person, on every node, leaves the room at once (`reauthorize_removes_a_member_who_lost_access_at_once`).
- **Add a typing indicator** (or cursors, "is viewing"): an `In` variant with no payload, answered with `out.broadcast_from(Out::Typing { … })` (everyone but the sending connection) and filtered in `handle_out` for the typist's other tabs; throttle it in the room (`Pace`) and in the client (`TYPING_EVERY` in `static/live_room.js`). Ephemeral events have no `seq` and are never replayed.
- **Scale to many nodes**: `APP_CLUSTER=true` (Postgres): each node joins `LISTEN offrails_realtime` and shares broadcasts, presence, revocations and re-authorizations; a load balancer needs no sticky sessions (clients resume from their cursor on any node). Each node keeps its own per-person cap (`max_conns_per_user`, `// APP:` in `kernel/channels.rs`) and per-room rate limits. Beyond a few thousand cluster messages a second, `NOTIFY` is the bottleneck: the escape hatch is a node-to-node mesh, recorded in `docs/decisions.md`.
- **Clients on another origin** (a native app, a SPA elsewhere): set `APP_REALTIME_SECRET` and `APP_REALTIME_ORIGINS`; the client gets a token from `POST /socket/token` (or your API mints one with `kernel::socket_token::mint` after its own authentication) and opens `new WebSocket(url, ["offrails.v1", "offrails.token." + token])` within 60 s. The token works once and dies with its session.

## Do not change

- Every join, push, revalidation and `reauthorize` goes through `Channels::authorize`, and unknown topics are refused (`joining_someone_elses_topic_is_forbidden`, `unknown_topics_are_refused_by_default`, `a_push_after_losing_access_is_forbidden_and_leaves`).
- A cookie handshake needs APP_URL's exact `Origin`, a missing one included (`a_foreign_or_missing_origin_is_refused_at_the_handshake`); relays refuse cross-site GETs and POSTs (`cross_site_requests_are_refused_on_every_transport`).
- The person comes from the session, never from a frame: pushes are `deny_unknown_fields` (`a_push_cannot_post_as_someone_else`).
- Revocation is keyed by person and each connection re-checks its own session (`logout_closes_that_sessions_sockets_only`, `an_expired_session_is_closed_by_revalidation`); `run` subscribes before it checks the session once more.
- Subscribe before computing a join's reply; store before emitting (`a_reconnect_replays_exactly_what_was_missed`, `a_message_is_stored_before_anyone_sees_it`).
- Relays are bound to person, session and a 256-bit token compared in constant time (`relays_need_their_owner_session_and_token`).
- The buffer sizes in `kernel::channels::upgrade` and `TOPIC_BUFFER`; the caps in `SocketConfig::new` (`the_connection_cap_refuses_the_next_socket_with_1008`, `flooding_closes_the_socket_with_1008`, `an_oversized_frame_closes_the_socket`); writes are bounded by `send_timeout` (`a_client_that_stops_reading_is_dropped`).
- Socket tokens: signature first, short-lived, single use, bound to the session (`a_socket_token_works_once`, `a_forged_or_altered_socket_token_is_refused`, `an_expired_socket_token_is_refused`).
- `/dev/sockets` and the guests are development only; the frame log is off elsewhere (`development_guests_and_dev_sockets_do_not_exist_in_production_or_test`, `the_frame_log_is_off_outside_development`).
- Text people typed is rendered with `textContent` in `static/live_room.js` and escaped by Askama on the page (`html_in_messages_is_delivered_raw_and_rendered_escaped`).

## How it works

```
browser tab ── WebSocket /socket (offrails.v1) ──▶ kernel/channels.rs  run(): one task per connection
          └─ or SSE /socket/sse + POST ───────────▶ kernel/transports.rs relays around the same run()
          └─ or long poll /socket/poll ───────────▶
                                       │ join / push (authorized by src/channels/mod.rs)
                                       ▼
                                 kernel/hub.rs  Hub<AppRoom>: a broadcast sender per topic,
                                 presence by person, a room actor per topic, revocations by person
                                       │ store first                    │ other nodes
                                       ▼                                ▼
                                 room_messages (seq = id)       kernel/cluster.rs: pg_notify / LISTEN
```

A **room** (`src/channels/live_room.rs`) is a tokio task per topic that runs one command at a time under supervision: a panic answers `unavailable`, counts a restart and rebuilds the state. A push is deserialized into the room's `In` enum before the room sees it (anything else is `bad_event`); what the room emits is serialized once and shared by every subscriber, except the events it intercepts (`handle_out`). Durable events carry `seq`, the message's row id; a client rejoins with `since` and gets exactly what it missed, from the database, so any node can answer (the guide's owner-node design isn't needed when replays read the database; per-room rate limits are therefore per node). Presence refs are `(node, connection)`: a person joins with their first ref and leaves with their last.

**One connection, any transport.** `kernel/channels.rs::run` is the whole protocol: limits, authorization, the join sequence (`ok`, the replay, `here`, then live diffs), heartbeats (WebSocket pings every 20 s; the client asks with `ping` after 25 s of silence), revalidation every 60 s, revocation, resync and shutdown. The WebSocket and both relays are `Pipe`s around it. `static/channels.js` tries the transport that worked last in the tab, moves on when one doesn't open within 5 s, reconnects with jittered backoff (250 ms → 10 s, ±25%), rejoins every channel with its cursor, buffers presence diffs until the new snapshot, drops events at or below its cursor, stops on `bye revoked` and fires `offrails:revoked`.

**More than one node** (`kernel/cluster.rs`, `APP_CLUSTER=true`). Each node publishes, from one task and in order, what its own connections do: `Fanout`, `Track`/`Untrack`, `Revoke`, `Reauthorize`, plus `Heartbeat` every 2 s and `Bye` on shutdown. A new node says `Hello`, every node answers with a complete `Sync` of its refs (chunked under the 8,000-byte `NOTIFY` limit), which replaces what the others knew about it. A node silent for 6 s is dead: its people leave. A lost `LISTEN` connection means lost messages: the node asks for fresh syncs and closes its sockets with 1012, so clients resume from their cursors. On SQLite the same messages travel an in-process bus (`LocalBus`), which is how the multi-node tests run on both databases.

| Route | Does |
|-------|------|
| `GET /socket` | the WebSocket (`offrails.v1`); a socket token as a second subprotocol |
| `GET /socket/sse`, `POST /socket/sse/{id}` | the SSE relay: `hello` `{conn, token}`, then one `frame` event per frame; frames go up by POST with `x-offrails-token` |
| `POST /socket/poll`, `GET`/`POST /socket/poll/{id}` | the long-poll relay: `{conn, token}`; GET waits ≤ 25 s and returns a JSON array |
| `POST /socket/token` | `{token, expires_in, protocol}` for this session (60 s, single use); 404 without `APP_REALTIME_SECRET` |
| `GET /live`, `/live/{name}` | the live room (redirects to sign in when signed out) |
| `GET /live/{name}/header`, `POST /live/{name}/headline` | the header region, and the edit that refreshes it on every open page |
| `GET`/`POST /dev/guests/sign-in`, `POST /dev/guests/sign-out` | development guests |
| `GET /dev/sockets`, `/dev/sockets/live`, `/dev/sockets.json` | the dev panel, its live part, and the same as JSON for agents |

Close codes: 1001 idle · 1002 protocol (also: no `offrails.v1`) · 1003 binary · 1008 rate limited or over the connection cap · 1009 frame too large · 1012 restart (reconnect, maybe elsewhere) · 1013 too slow · **4401 session revoked or expired (don't reconnect)**. `err` codes: `forbidden`, `unknown_topic`, `not_joined`, `already_joined`, `invalid`, `rate_limited`, `bad_event`, `limit`, `unavailable`.

## Files

| File | What |
|------|------|
| `src/kernel/hub.rs` | the hub: `Room`, `Outbox`, topics, presence, room actors, revocations, caps, frame log, snapshot |
| `src/kernel/channels.rs` | `offrails.v1`: typed frames, `Channels`, `SocketConfig`, the connection loop, the WebSocket transport, origin rules |
| `src/kernel/transports.rs` | the SSE and long-poll relays |
| `src/kernel/cluster.rs` | the bus between nodes: Postgres `LISTEN/NOTIFY` or in-process |
| `src/kernel/socket_token.rs` | signed, single-use socket tokens |
| `src/kernel/sockets_web.rs` | `/dev/sockets` |
| `src/channels/mod.rs` | the app's side: routes, `authorize`, `build_hub`, `session_ended`, `refresh` |
| `src/channels/live_room.rs` | the example room (`AppRoom`, `LiveRoom`) |
| `src/channels/identity.rs` | who is connecting: the people source (development guests, or accounts re-exported over them) |
| `src/channels/accounts.rs` | people as auth-core accounts; built only when auth-core is installed (optional wiring) |
| `src/controllers/live.rs`, `src/views/live.rs` | the live room's pages and the development sign-in |
| `src/models/{guest,guest_session,room,room_message}.rs` | guests, rooms and members, messages |
| `migration/src/m20261016_0050{00,100,200}_*.rs` | guests, rooms (seeded: lobby, design, backstage), socket token uses |
| `templates/live/*.html`, `templates/kernel/sockets/*.html` | the pages |
| `static/channels.js` | the client: connection, transports, channels, presence, refresh |
| `static/live_room.js`, `static/kernel/sockets.css` | the live room's behavior; the dev panel's styles |
| `tests/app/realtime_test/` | the tests named in `CHECKS.md` (protocol, security, tokens, presence, resume, transports, cluster, pages); `people.rs` signs people in (guests, or `people_accounts.rs` with auth-core), `accounts.rs` runs with auth-core only |
| `tests/e2e/live_room.e2e.mjs`, `tests/e2e/live.scenario.mjs` | the two-person browser test; the scenario for the framework's live-tabs and a11y smokes |
