//! Realtime (samples/realtime): the offrails.v1 protocol, its security, presence, resume, the
//! fallback transports, more than one node, and the live room's pages. Every test runs on SQLite
//! and, with `TEST_DATABASE_URL`, on Postgres. Sockets use a real port; nothing sleeps: waits are
//! bounded reads (`client::WAIT`) or `wait_until`.
//!
//! GUIDE: .offrails/guides/channels.md#testing

mod client;
mod cluster;
mod live_room;
mod people;
mod presence;
mod protocol;
mod resume;
mod security;
mod tokens;
mod transports;
// samples: realtime test mods
