# realtime: checks

What "working and safe" means for channels, and the test that proves each item. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh realtime` runs them all on SQLite and Postgres (the cluster tests talk over `LISTEN/NOTIFY` on Postgres and over the in-process bus on SQLite).

A realtime connection is authenticated once and then lives for hours, through logouts, permission changes and deploys. Every rule a request gets for free (CSRF, a session lookup, authorization) has to hold for the connection's whole life. Treat every frame as hostile and every client as scripted.

## Threat model

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | **Cross-site WebSocket hijacking**: a page on another site opens a socket to the app and the browser attaches the session cookie (CSRF protection doesn't cover WebSocket handshakes) | A cookie handshake needs `Origin` exactly equal to APP_URL's origin; a missing `Origin` is refused too; cookies are `SameSite=Lax` | `a_foreign_or_missing_origin_is_refused_at_the_handshake`, `the_handshake_origin_rule` |
| Spoofing | The same, through the fallbacks: a cross-site page opens the SSE stream or a long-poll relay, or posts frames to one | GETs refuse `Sec-Fetch-Site: cross-site`/`same-site` and any foreign `Origin`; POSTs go through the CSRF layer | `cross_site_requests_are_refused_on_every_transport`, `cross_site_gets_are_recognised` |
| Spoofing | **Impersonation**: a client sends someone else's user id or name in a frame | The person comes from the server-side session only (`identity.rs`); pushes are typed with `deny_unknown_fields`, so an `author_id` is refused, never read; messages are stored with the session's person | `a_push_cannot_post_as_someone_else`, `pushes_are_typed_and_refuse_extra_fields` |
| Spoofing | A cookie planted by a sibling subdomain signs a socket in (session fixation) | Production reads only `__Host-session_token` | `production_reads_only_the_host_prefixed_session_cookie` |
| Spoofing | A forged or edited socket token signs in as someone else | HMAC-SHA256 under `APP_REALTIME_SECRET`, verified in constant time before anything in it is trusted; tokens are off without a secret | `a_forged_or_altered_socket_token_is_refused`, `tokens_signed_with_another_secret_or_altered_are_refused`, `minting_needs_a_session_a_same_origin_post_and_a_secret` |
| Spoofing | A token-authenticated socket from a hostile page | Token sockets accept no `Origin` (native clients) or one listed in `APP_REALTIME_ORIGINS`; a cookie never rides along (the token decides who it is) | `token_sockets_follow_their_own_origin_rule_and_ignore_cookies` |
| Tampering | Malformed frames, unknown frame types, arrays, binary frames crash or confuse the server | Frames are typed (`ClientFrame`); anything else is `bye protocol` and close 1002 (1003 for binary); topics and refs are short and plain | `garbage_unknown_types_and_arrays_close_with_1002`, `binary_frames_close_with_1003`, `client_frames_are_typed_and_tolerant_of_new_fields`, `topics_and_refs_stay_short_and_plain` |
| Tampering | A push of an event the room doesn't declare reaches room code | Every push is deserialized into the room's `In` enum before the room runs: `bad_event` otherwise | `an_undeclared_event_is_bad_event_and_invalid_data_says_why`, `undeclared_events_are_refused_before_the_room_runs` |
| Tampering | Markup in a message runs in other people's pages (stored XSS) | Messages are stored and sent as raw text; the page renders them escaped (Askama) and the client with `textContent` from `<template>` clones; strict CSP, no inline script | `html_in_messages_is_delivered_raw_and_rendered_escaped`, `the_room_renders_its_history_escaped_with_the_cursor_the_socket_resumes_from` |
| Tampering | A message everyone saw doesn't exist (broadcast before a failed write) | Stored first, then emitted | `a_message_is_stored_before_anyone_sees_it` |
| Repudiation | Someone denies a message, or posts as another person | The author is the session's person, stored with the message; the server never takes an author from the client | `a_push_cannot_post_as_someone_else` |
| Information disclosure | **Topic enumeration**: probing `room:<slug>` or `user:<id>` to learn what exists | A missing room and a closed one give the same `forbidden`; unknown kinds are `unknown_topic`; the page answers 404 for both | `a_members_only_room_and_a_missing_room_look_the_same`, `a_members_only_room_is_not_found_for_others` |
| Information disclosure | Reading someone else's private topic (`user:<id>`) | `authorize` accepts a private topic for its owner only, by exact id; deny by default for every unknown topic | `joining_someone_elses_topic_is_forbidden`, `unknown_topics_are_refused_by_default` |
| Information disclosure | Presence leaks personal data | Presence metadata is a display name, initials and a color, never an email (with auth-core accounts a neutral "Member N", nothing derived from the address); only members who passed `authorize` receive a topic's presence | `presence_carries_names_never_emails`, `presence_is_per_topic`, `an_accounts_presence_shows_a_name_never_the_email` |
| Information disclosure | Someone removed from a room keeps receiving it | `authorize` runs on every push, at every revalidation and on `Hub::reauthorize` (every node); a refusal leaves the topic | `a_push_after_losing_access_is_forbidden_and_leaves`, `reauthorize_removes_a_member_who_lost_access_at_once`, `revalidation_also_removes_topics_the_member_lost`, `reauthorize_crosses_nodes` |
| Information disclosure | Message text in production memory, or a dev panel in production | The frame log is on in development only; `/dev/sockets` and the guest sign-in exist only in development; every `/dev/sockets` response is `no-store` | `the_frame_log_is_off_outside_development`, `development_guests_and_dev_sockets_do_not_exist_in_production_or_test`, `every_sockets_response_is_no_store` |
| Information disclosure | A relay's frames read by someone else | A relay is bound to its person, its session and a 256-bit token in a header, compared in constant time; an unknown id looks like someone else's (403) | `relays_need_their_owner_session_and_token`, `constant_time_eq_compares_whole_strings` |
| Denial of service | **Flooding**: a client sends frames as fast as it can | More than 100 frames in 10 s: `bye rate_limited`, close 1008; posts are limited per person per room (10 per 10 s, answered `rate_limited` without closing) | `flooding_closes_the_socket_with_1008`, `the_per_room_post_limit_answers_rate_limited_without_closing` |
| Denial of service | Oversized frames exhaust memory | 4 KiB per frame: the WebSocket reader refuses it (close), a relay POST gets 413; bodies are capped at 1,000 characters | `an_oversized_frame_closes_the_socket`, `an_oversized_relay_post_is_413`, `bodies_are_trimmed_bounded_and_stripped_of_control_characters` |
| Denial of service | One person opens thousands of connections or topics | 128 connections per person per node on every transport (checked and counted under one lock), 20 topics and 30 joins a minute per connection | `the_connection_cap_refuses_the_next_socket_with_1008`, `the_relay_cap_answers_429`, `the_cap_is_checked_and_counted_under_one_lock`, `caps_on_topics_and_joins_per_minute_hold` |
| Denial of service | A slow or dead client makes the server buffer for everyone | Each topic buffers 256 frames per subscriber: `slow` at 3/4, then `bye too_slow` and close 1013; a socket write that stalls for 10 s ends the connection (a client that stops reading can't park it); a silent socket closes after 60 s; relays never wait on a client | `a_slow_consumer_is_warned_then_dropped`, `a_client_that_stops_reading_is_dropped`, `a_silent_client_is_closed_as_idle` |
| Denial of service | Waves used to ping arbitrary people | A wave reaches only someone present in the same room, not yourself, 5 a minute | `a_wave_reaches_only_its_target_on_their_private_topic` |
| Elevation of privilege | Joining a members-only room | `room::admits` against the database at every join and push | `a_members_only_room_and_a_missing_room_look_the_same` |
| Elevation of privilege | A revoked or expired session keeps its sockets open | Sign-out wakes that person's connections (on every node); each re-checks its own session and closes with `bye revoked` (4401); expiry closes at the next revalidation; the connection re-checks once more right after it starts listening | `logout_closes_that_sessions_sockets_only`, `signing_out_everywhere_closes_every_socket_of_that_person`, `an_expired_session_is_closed_by_revalidation`, `logout_ends_relays_with_bye`, `a_revocation_on_one_node_closes_sockets_on_another`, `development_guests_sign_in_and_signing_out_closes_their_sockets` |
| Elevation of privilege | **Replay**: a captured socket token used again, or after its session ended | Tokens are single use on every node (the nonce's digest is stored until expiry), live 60 s, can't claim a longer life, and are bound to their session | `a_socket_token_works_once`, `an_expired_socket_token_is_refused`, `a_far_future_expiry_is_refused_even_when_signed`, `a_token_from_an_ended_session_is_refused_and_a_token_socket_closes_when_it_ends` |
| Elevation of privilege | Replay of old events to rewrite what a client shows | Events carry the stored `seq`; clients drop anything at or below their cursor; replays come from the database for that room only | `a_reconnect_replays_exactly_what_was_missed`, `replays_belong_to_their_room` |

## Invariants

| Invariant | Test |
|-----------|------|
| A join answers `ok` (with the room's cursor), then the replay, then `here` including the joiner | `a_join_answers_ok_with_the_cursor_then_presence_here_including_the_joiner` |
| A reconnect with `since` gets exactly the missed messages, oldest first; too many is a `gap`, a cursor from the future too | `a_reconnect_replays_exactly_what_was_missed`, `too_much_missed_is_a_gap`, `a_join_without_a_cursor_replays_nothing` |
| Presence lists people, not tabs; a person leaves with their last connection | `two_tabs_are_one_person_until_the_last_one_leaves`, `presence_lists_people_not_tabs`, `a_closed_socket_leaves_presence` |
| `broadcast_from` skips the sender; `handle_out` skips the typist's other tabs; typing is never replayed | `typing_reaches_the_others_but_none_of_the_typists_tabs`, `a_typists_other_tabs_never_hear_themselves` |
| A broadcast is serialized once for every subscriber | `a_broadcast_is_serialized_once_for_every_subscriber` |
| One command at a time per room; a panicking room answers `unavailable` and rebuilds | `commands_run_one_at_a_time`, `a_panicking_room_answers_unavailable_and_rebuilds` |
| Revocations wake only that person; their sender is swept with their last connection | `revocation_wakes_only_that_member_and_its_sender_is_swept` |
| SSE + POST and long polling speak the same protocol and talk to WebSocket clients | `sse_and_post_speak_the_protocol_and_talk_to_websocket_clients`, `long_polling_speaks_the_protocol_too`, `transports_can_be_switched_off`, `relay_responses_are_no_store` |
| Nodes share presence, messages, typing; a dead node's people leave; a clean stop leaves at once; a new node learns who's here | `presence_messages_and_typing_cross_nodes`, `a_dead_nodes_people_leave_after_its_heartbeats_stop`, `a_clean_shutdown_leaves_at_once`, `a_node_that_starts_later_learns_who_is_here` |
| Sync lists fit a `NOTIFY` | `sync_lists_are_cut_into_chunks_that_fit_a_notify` |
| Shutdown says `restart` and closes 1012, so clients come back | `shutdown_says_restart_and_closes_1012` |
| The htmx refresh helper makes every page in a room re-fetch its region through the authorized GET | `editing_the_headline_refreshes_the_header_on_every_open_page` |
| An idle connection's task stays small (10,000 sockets) | `an_idle_connection_future_stays_small` |
| Links into the live room and `/dev/sockets` aren't boosted (their scripts and styles load in `<head>`) | `links_into_the_live_room_and_dev_sockets_are_not_boosted` |
| `/dev/sockets` shows connections, topics, presence and frames | `dev_sockets_shows_connections_topics_presence_and_frames` |
| With auth-core, people are accounts: signing out through auth-core, or "sign out everywhere else", closes those sessions' sockets at once (the realtime suite then runs on accounts) | `signing_out_through_auth_core_closes_that_sessions_sockets_at_once`, `signing_out_everywhere_else_closes_the_other_devices_sockets`, `every_session_ending_path_calls_sessions_ended` |
| With auth-core, presence shows a neutral "Member N" (or the app's own name column), never the address nor a name derived from it; the guest pages send people to sign in and never touch an account's session | `an_accounts_presence_shows_a_name_never_the_email`, `with_accounts_the_guest_pages_send_people_to_sign_in` |
| With auth-core, signing in over another account's session in the same browser closes that session's sockets at once | `signing_in_over_a_session_closes_its_sockets`, `every_session_ending_path_calls_sessions_ended` |
| The live room, the guest sign-in and `/dev/sockets` follow the kernel theme switch (`data-theme` from the cookie); `sockets.css` uses the kernel's `light-dark()` tokens, no `prefers-color-scheme` block | `the_live_room_and_dev_sockets_follow_the_theme_switch`, `every_kernel_page_renders_data_theme` |

## In a browser (not part of the test suite)

The e2e scripts sign in as development guests (realtime alone). With auth-core installed, `tools/screenshots.sh realtime` drives the same room with three accounts signed in by emailed code, and fails on any console error.

- Two people in two browser contexts see each other's presence, typing, messages (as text) and waves; a third whose WebSocket is blocked runs on SSE, a fourth with SSE blocked too on long polling, and all four talk; signing out closes the page's connection and the others see the person leave; no console errors (CSP included): `BASE=… node tests/e2e/live_room.e2e.mjs`.
- Every tab updates: `SCENARIO=tests/e2e/live.scenario.mjs BASE=… TABS=40 bash .offrails/tools/browser-smoke/run.sh live-tabs` (serve HTTPS for HTTP/2; over HTTP/1.1 tabs past ~6 stall by design of the browser).
- axe-core (WCAG 2.2 AA), light and dark, on the sign-in, two rooms and `/dev/sockets`: `SCENARIO=tests/e2e/live.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh a11y`.

```sh
tools/demo.sh realtime --port 3910
BASE=http://localhost:3910 node samples/realtime/files/tests/e2e/live_room.e2e.mjs
SCENARIO=samples/realtime/files/tests/e2e/live.scenario.mjs BASE=http://localhost:3910 bash host/.offrails/tools/browser-smoke/run.sh a11y
tools/demo.sh stop
```

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh realtime   # alone, on both databases
TEST_DATABASE_URL=postgres://… tools/ci.sh all        # with every other sample
tools/security-check.sh
```
