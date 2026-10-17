//! The fallbacks: Server-Sent Events + POST, then long-polling, for networks whose proxies break
//! WebSockets. Both are relays around the same connection loop (`channels::run`).
//!
//! GUIDE: .offrails/guides/channels.md#transports
//!
//! | Transport | Down | Up |
//! |-----------|------|----|
//! | SSE | `GET /socket/sse`: event `hello` `{conn, token}`, then one `frame` event per frame | `POST /socket/sse/{conn}` |
//! | Long-poll | `POST /socket/poll` → `{conn, token}`; `GET /socket/poll/{conn}` waits ≤ 25 s | `POST /socket/poll/{conn}` |
//!
//! A relay is bound to the person and session that opened it *and* a random 256-bit token sent
//! in the `x-offrails-token` header, compared in constant time: another person, another session,
//! or the same person without the token gets 403. POSTs pass the app's CSRF layer; GETs refuse
//! cross-site requests.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::stream::{self, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::kernel::channels::{self, Channels, Inbound, Pipe, is_cross_site};
use crate::kernel::hub::{Frame, Member};

/// The header a relay's token travels in.
pub const TOKEN_HEADER: &str = "x-offrails-token";
/// Frames a relay holds for its client (SSE stream or the next poll) before the client counts
/// as too slow and the connection ends.
const RELAY_BUFFER: usize = 512;
/// Client frames queued from POSTs before a POST gets 429.
const RELAY_INBOX: usize = 64;
/// A long-poll client with no GET waiting for this long has left.
const POLL_ABANDONED: Duration = Duration::from_secs(10);
/// An ended relay stays readable this long, so its client collects the final `bye`.
const ENDED_GRACE: Duration = Duration::from_secs(30);

enum RelayOut {
    Frame(Frame),
    Close,
}

#[derive(Clone)]
struct Relay {
    owner: String,
    session: Option<i64>,
    token: String,
    inbox: mpsc::Sender<Inbound>,
    outbox: Arc<tokio::sync::Mutex<mpsc::Receiver<RelayOut>>>,
    last_active: Arc<Mutex<Instant>>,
    /// Long-poll GETs waiting right now.
    polling: Arc<AtomicUsize>,
    ended: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Relay {
    fn touch(&self) {
        *lock(&self.last_active) = Instant::now();
    }
}

/// The open relays of this node, by connection id.
#[derive(Clone, Default)]
pub struct Relays {
    inner: Arc<Mutex<HashMap<u64, Relay>>>,
}

impl Relays {
    pub fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, conn: u64) -> Option<Relay> {
        lock(&self.inner).get(&conn).cloned()
    }

    fn insert(&self, conn: u64, relay: Relay) {
        lock(&self.inner).insert(conn, relay);
    }

    fn remove(&self, conn: u64) {
        lock(&self.inner).remove(&conn);
    }
}

struct RelayPipe {
    inbox: mpsc::Receiver<Inbound>,
    out: mpsc::Sender<RelayOut>,
}

impl Pipe for RelayPipe {
    const HEARTBEATS: bool = false;

    async fn recv(&mut self) -> Inbound {
        self.inbox.recv().await.unwrap_or(Inbound::Closed)
    }

    async fn send(&mut self, frame: Frame) -> bool {
        // WHY: never wait on a client: a full buffer means it isn't reading; end it, and it
        // resumes from its cursor.
        self.out.try_send(RelayOut::Frame(frame)).is_ok()
    }

    async fn ping(&mut self) -> bool {
        true
    }

    async fn finish(self, bye: Option<Frame>, _code: u16) {
        if let Some(bye) = bye {
            let _ = self.out.try_send(RelayOut::Frame(bye));
        }
        let _ = self.out.try_send(RelayOut::Close);
    }
}

/// Equal, in time that doesn't depend on where they differ.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Sse,
    Poll,
}

/// Starts a connection loop behind a relay and registers it.
fn open_relay<S: Channels>(s: &S, member: Member, kind: Kind) -> Result<(u64, Relay), StatusCode> {
    let cfg = s.hub().config();
    let transport = match kind {
        Kind::Sse => "sse",
        Kind::Poll => "longpoll",
    };
    // SECURITY: the per-person cap holds on every transport; relays answer 429 (test: the_relay_cap_answers_429).
    let guard = s
        .hub()
        .try_register(&member, transport, cfg.max_conns_per_user)
        .ok_or(StatusCode::TOO_MANY_REQUESTS)?;
    let conn = guard.id();
    let (in_tx, in_rx) = mpsc::channel(RELAY_INBOX);
    let (out_tx, out_rx) = mpsc::channel(RELAY_BUFFER);
    let relay = Relay {
        owner: member.key.clone(),
        session: member.session,
        // SECURITY: 256 random bits per relay, compared in constant time (test: relays_need_their_owner_session_and_token).
        token: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
        inbox: in_tx,
        outbox: Arc::new(tokio::sync::Mutex::new(out_rx)),
        last_active: Arc::new(Mutex::new(Instant::now())),
        polling: Arc::new(AtomicUsize::new(0)),
        ended: Arc::new(AtomicBool::new(false)),
    };
    let relays = s.hub().relays().clone();
    relays.insert(conn, relay.clone());

    let pipe = RelayPipe {
        inbox: in_rx,
        out: out_tx,
    };
    let session = tokio::spawn(channels::run(s.clone(), pipe, member, guard));
    let ended = relay.ended.clone();
    let cleanup = relays.clone();
    tokio::spawn(async move {
        let _ = session.await;
        ended.store(true, Ordering::SeqCst);
        // Frames queued before the end (the `bye`) stay readable for a moment.
        tokio::time::sleep(ENDED_GRACE).await;
        cleanup.remove(conn);
    });

    if kind == Kind::Poll {
        // A client that stopped polling has left: end its connection instead of keeping it.
        let watched = relay.clone();
        let relays = s.hub().relays().clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if relays.get(conn).is_none() || watched.ended.load(Ordering::SeqCst) {
                    break;
                }
                let waiting = watched.polling.load(Ordering::SeqCst) > 0;
                let idle = lock(&watched.last_active).elapsed();
                if !waiting
                    && idle > POLL_ABANDONED
                    && watched.inbox.try_send(Inbound::Closed).is_ok()
                {
                    break;
                }
            }
        });
    }
    Ok((conn, relay))
}

/// The relay `conn`, if it belongs to this member's session and the request carries its token.
fn authorized_relay<S: Channels>(
    s: &S,
    headers: &HeaderMap,
    member: &Member,
    conn: u64,
) -> Result<Relay, StatusCode> {
    // WHY: an unknown id and someone else's relay look the same (403): ids reveal nothing.
    let relay = s.hub().relays().get(conn).ok_or(StatusCode::FORBIDDEN)?;
    let token = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if relay.owner != member.key
        || relay.session != member.session
        || !constant_time_eq(&relay.token, token)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    relay.touch();
    Ok(relay)
}

/// Closes the connection when the SSE response body is dropped (the client went away).
struct CloseOnDrop(mpsc::Sender<Inbound>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if self.0.try_send(Inbound::Closed).is_err() {
            let inbox = self.0.clone();
            tokio::spawn(async move {
                let _ = inbox.send(Inbound::Closed).await;
            });
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /socket/sse`: event `hello` `{conn, token}`, then one `frame` event per frame.
pub fn sse_open<S: Channels>(s: S, headers: &HeaderMap, member: Member) -> Response {
    // SECURITY: browsers send cookies on cross-site GETs, which CSRF doesn't cover (test: cross_site_requests_are_refused_on_every_transport).
    if is_cross_site(&s.hub().config(), headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (conn, relay) = match open_relay(&s, member, Kind::Sse) {
        Ok(opened) => opened,
        Err(status) => return status.into_response(),
    };
    let hello = Event::default()
        .event("hello")
        .data(json!({ "conn": conn, "token": relay.token }).to_string());
    let guard = CloseOnDrop(relay.inbox.clone());
    let frames = stream::unfold(
        (relay.outbox.clone(), guard),
        |(outbox, guard)| async move {
            let next = outbox.lock().await.recv().await;
            match next {
                Some(RelayOut::Frame(f)) => {
                    let event = Event::default().event("frame").data(f.as_str());
                    Some((Ok::<Event, Infallible>(event), (outbox, guard)))
                }
                // After `bye`, the stream ends (the client closes its EventSource on `bye`).
                Some(RelayOut::Close) | None => None,
            }
        },
    );
    let body = stream::once(async move { Ok::<Event, Infallible>(hello) }).chain(frames);
    no_store(
        Sse::new(body)
            .keep_alive(KeepAlive::default())
            .into_response(),
    )
}

/// `POST /socket/poll`: `{conn, token}`.
pub fn poll_open<S: Channels>(s: S, headers: &HeaderMap, member: Member) -> Response {
    if is_cross_site(&s.hub().config(), headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match open_relay(&s, member, Kind::Poll) {
        Ok((conn, relay)) => {
            no_store(axum::Json(json!({ "conn": conn, "token": relay.token })).into_response())
        }
        Err(status) => status.into_response(),
    }
}

/// Counts a waiting long-poll GET; dropping it (answered, or the client left) restarts the
/// abandonment clock.
struct Waiting(Relay);

impl Waiting {
    fn new(relay: &Relay) -> Self {
        relay.polling.fetch_add(1, Ordering::SeqCst);
        Self(relay.clone())
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.touch();
        self.0.polling.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `GET /socket/poll/{conn}`: waits up to `poll_wait` for frames; returns them as a JSON array.
/// 410 once the connection ended and its last frames were collected.
pub async fn poll_get<S: Channels>(
    s: S,
    headers: &HeaderMap,
    member: Member,
    conn: u64,
) -> Response {
    if is_cross_site(&s.hub().config(), headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let relay = match authorized_relay(&s, headers, &member, conn) {
        Ok(r) => r,
        Err(status) => return status.into_response(),
    };
    let wait = s.hub().config().poll_wait;
    let _waiting = Waiting::new(&relay);
    let mut outbox = relay.outbox.lock().await;
    let mut frames: Vec<Frame> = Vec::new();
    let mut finished = false;
    match tokio::time::timeout(wait, outbox.recv()).await {
        Ok(Some(RelayOut::Frame(f))) => frames.push(f),
        Ok(Some(RelayOut::Close) | None) => finished = true,
        Err(_) => {}
    }
    while !finished && frames.len() < 200 {
        match outbox.try_recv() {
            Ok(RelayOut::Frame(f)) => frames.push(f),
            Ok(RelayOut::Close) => finished = true,
            Err(_) => break,
        }
    }
    drop(outbox);
    if finished && frames.is_empty() {
        s.hub().relays().remove(conn);
        return StatusCode::GONE.into_response();
    }
    // Frames are JSON already: join them instead of parsing and re-serializing.
    let body = format!(
        "[{}]",
        frames
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    no_store(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// `POST /socket/sse/{conn}` and `POST /socket/poll/{conn}`: one client frame. 202.
pub async fn relay_post<S: Channels>(
    s: S,
    headers: &HeaderMap,
    member: Member,
    conn: u64,
    body: String,
) -> Response {
    let relay = match authorized_relay(&s, headers, &member, conn) {
        Ok(r) => r,
        Err(status) => return status.into_response(),
    };
    // SECURITY: the same 4 KiB frame cap as the socket (test: an_oversized_relay_post_is_413).
    if body.len() > s.hub().config().max_frame {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if relay.ended.load(Ordering::SeqCst) {
        return StatusCode::GONE.into_response();
    }
    match relay.inbox.try_send(Inbound::Text(body.into())) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(mpsc::error::TrySendError::Full(_)) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(mpsc::error::TrySendError::Closed(_)) => StatusCode::GONE.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_compares_whole_strings() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "x"));
    }
}
