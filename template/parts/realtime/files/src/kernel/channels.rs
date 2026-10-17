//! One client connection speaking `offrails.v1`, whatever the transport (Phoenix.Socket).
//!
//! GUIDE: .offrails/guides/channels.md#the-connection-kernelchannelsrs
//!
//! Every frame, both ways, is a JSON object with a `t` field, typed below (`ClientFrame`,
//! `ServerFrame`). One task per connection `select!`s over the client's frames, the joined
//! topics, a timer (idle, ping, revalidation), the member's revocation signal, a cluster resync
//! and shutdown. WebSocket, SSE + POST and long-polling all run this same loop (`run`), so joins,
//! authorization, limits, resume and revocation are identical everywhere.
//!
//! Security holds for the connection's whole life, not once at the handshake: an exact `Origin`
//! (cross-site WebSocket hijacking), a session decided by the server, every join and push
//! authorized against the database now, the session re-checked on a timer and the moment its
//! person is revoked, and caps on connections, topics, joins, frames and frame size.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::kernel::hub::{
    ConnGuard, ConnInfo, Delivery, Frame, Hub, Member, Outgoing, Payload, Rejection, Room,
    TOPIC_BUFFER, event_frame, presence_here_frame,
};

/// The negotiated WebSocket subprotocol. A client that doesn't offer it gets `bye protocol`.
pub const PROTOCOL: &str = "offrails.v1";
/// A socket token travels as a second subprotocol, `offrails.token.<token>` (browsers can't set
/// headers on a WebSocket, and a token in the URL would land in access logs).
pub const TOKEN_PROTOCOL_PREFIX: &str = "offrails.token.";

/// Close codes. Every close is preceded by a `bye` frame saying why.
pub mod close {
    pub const IDLE: u16 = 1001;
    pub const PROTOCOL: u16 = 1002;
    pub const BINARY: u16 = 1003;
    pub const POLICY: u16 = 1008;
    pub const TOO_BIG: u16 = 1009;
    pub const RESTART: u16 = 1012;
    pub const TOO_SLOW: u16 = 1013;
    /// The session was revoked or expired: don't reconnect.
    pub const REVOKED: u16 = 4401;
}

/// Limits and timings of every connection.
#[derive(Clone, Debug)]
pub struct SocketConfig {
    /// Origins a browser may open a socket or stream from: APP_URL's (and the development
    /// plain-http port's). Exact match.
    pub allowed_origins: Vec<String>,
    /// Extra origins allowed for token-authenticated sockets (`APP_REALTIME_ORIGINS`).
    pub token_origins: Vec<String>,
    /// Which transports this server offers (`APP_REALTIME_TRANSPORTS`).
    pub transports: Vec<String>,
    /// Bytes per frame, both ways.
    pub max_frame: usize,
    pub max_in_per_10s: u32,
    pub idle_timeout: Duration,
    pub ping_every: Duration,
    pub revalidate_every: Duration,
    /// Connections per person on this node, all transports. Sized for tabs: each tab can hold
    /// two (a stream and a socket), and a cap of 8 broke people at about five tabs.
    pub max_conns_per_user: usize,
    pub max_topics: usize,
    pub max_joins_per_min: u32,
    /// How long a long-poll GET waits for frames.
    pub poll_wait: Duration,
    /// A client that doesn't take a frame within this long has stopped reading: dropped, so a
    /// stuck write can't hold the connection (and its revocation) forever.
    pub send_timeout: Duration,
}

impl SocketConfig {
    pub fn new(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins,
            token_origins: Vec::new(),
            transports: vec!["websocket".into(), "sse".into(), "longpoll".into()],
            max_frame: 4 * 1024,
            max_in_per_10s: 100,
            idle_timeout: Duration::from_secs(60),
            ping_every: Duration::from_secs(20),
            revalidate_every: Duration::from_secs(60),
            max_conns_per_user: 128,
            max_topics: 20,
            max_joins_per_min: 30,
            poll_wait: Duration::from_secs(25),
            send_timeout: Duration::from_secs(10),
        }
    }

    pub fn offers(&self, transport: &str) -> bool {
        self.transports.iter().any(|t| t == transport)
    }
}

/// What the app decides (implemented by `AppState` in `src/channels/mod.rs`); the rest is the
/// kernel's.
pub trait Channels: Clone + Send + Sync + 'static {
    type Room: Room<Ctx = Self>;
    fn hub(&self) -> &Hub<Self::Room>;
    /// May `member` be on `topic`, against the database as it is now? Called on every join, on
    /// every push, at every revalidation and on `Hub::reauthorize`. Deny by default.
    fn authorize(
        &self,
        member: &Member,
        topic: &str,
    ) -> impl Future<Output = Result<(), Rejection>> + Send;
    /// Is this connection's session still live? Fails closed.
    fn session_valid(&self, member: &Member) -> impl Future<Output = bool> + Send;
}

// ---- frames -----------------------------------------------------------------------------------

/// A client's `ref`, echoed in the reply: a number or a short string.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Ref {
    Num(u64),
    Str(String),
}

/// What a client may send. Unknown fields are ignored (forward compatibility); an unknown `t`,
/// non-JSON or an array is a protocol error.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientFrame {
    Join {
        #[serde(rename = "ref")]
        reference: Option<Ref>,
        topic: String,
        since: Option<i64>,
    },
    Leave {
        #[serde(rename = "ref")]
        reference: Option<Ref>,
        topic: String,
    },
    Push {
        #[serde(rename = "ref")]
        reference: Option<Ref>,
        topic: String,
        event: String,
        #[serde(default)]
        data: Payload,
    },
    Ping {
        #[serde(rename = "ref")]
        reference: Option<Ref>,
    },
}

/// What the server sends (besides `event` and `presence`, which the hub builds once per
/// broadcast).
#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame<'a> {
    Ok {
        #[serde(rename = "ref")]
        reference: Option<&'a Ref>,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<&'a str>,
        data: &'a Payload,
        #[serde(skip_serializing_if = "Option::is_none")]
        replay: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gap: Option<bool>,
    },
    Err {
        #[serde(rename = "ref")]
        reference: Option<&'a Ref>,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<&'a str>,
        code: &'a str,
        message: &'a str,
    },
    Pong {
        #[serde(rename = "ref")]
        reference: Option<&'a Ref>,
    },
    Slow {
        topic: &'a str,
    },
    Bye {
        code: &'a str,
        reason: &'a str,
    },
}

impl ServerFrame<'_> {
    pub fn frame(&self) -> Frame {
        serde_json::to_string(self).unwrap_or_default().into()
    }
}

fn err_frame(reference: Option<&Ref>, topic: Option<&str>, r: &Rejection) -> Frame {
    ServerFrame::Err {
        reference,
        topic,
        code: &r.code,
        message: &r.message,
    }
    .frame()
}

/// Topic names are short and plain: `room:lobby`, `user:7`.
fn plain_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic.len() <= 128
        && topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-' | b'.'))
}

fn plain_ref(r: &Option<Ref>) -> bool {
    match r {
        Some(Ref::Str(s)) => s.len() <= 64,
        _ => true,
    }
}

// ---- origins ----------------------------------------------------------------------------------

/// `scheme://host[:port]` of an absolute URL: what a browser sends as `Origin`.
pub fn origin_of(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            format!("{scheme}://{host}")
        }
        None => url.to_owned(),
    }
}

/// The WebSocket handshake's `Origin` rule.
///
/// Browsers send cookies on cross-site WebSocket handshakes and CSRF protection doesn't cover
/// them (cross-site WebSocket hijacking), so a cookie-authenticated socket needs an `Origin`
/// that is exactly one of `allowed_origins`; a missing one is refused too (browsers always send
/// it). A token-authenticated socket carries no ambient credential: it may come without an
/// `Origin` (a native client) or from one of `token_origins`.
pub fn origin_allowed(cfg: &SocketConfig, headers: &HeaderMap, via_token: bool) -> bool {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    match origin {
        Some(o) => {
            cfg.allowed_origins.iter().any(|a| a == o)
                || (via_token && cfg.token_origins.iter().any(|a| a == o))
        }
        None => via_token,
    }
}

/// For the HTTP transports' GETs (CSRF protection covers only unsafe methods): refuse a
/// browser that says the request is cross-site, and any foreign `Origin`.
pub fn is_cross_site(cfg: &SocketConfig, headers: &HeaderMap) -> bool {
    let site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if site == "cross-site" || site == "same-site" {
        return true;
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => !cfg.allowed_origins.iter().any(|a| a == origin),
        None => false,
    }
}

/// The socket token a client offered as a subprotocol, if any.
pub fn offered_token(ws: &WebSocketUpgrade) -> Option<String> {
    ws.requested_protocols()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .find_map(|p| p.strip_prefix(TOKEN_PROTOCOL_PREFIX))
        .map(str::to_owned)
}

// ---- the WebSocket transport --------------------------------------------------------------------

/// `GET /socket` after the app checked the origin and found the member: the upgrade.
pub fn upgrade<S: Channels>(s: S, ws: WebSocketUpgrade, member: Member) -> Response {
    let cfg = s.hub().config();
    // GUIDE: .offrails/guides/channels.md#the-connection-kernelchannelsrs (per-socket buffers)
    // WHY: axum's defaults are 128 KiB read + 128 KiB write per socket, sized for throughput; at
    // 10,000 sockets that was 1.37 GB. Frames are ≤ 4 KiB: 160–225 MB with these.
    ws.protocols([PROTOCOL])
        .read_buffer_size(4 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(256 * 1024)
        .max_message_size(cfg.max_frame)
        .max_frame_size(cfg.max_frame)
        .on_upgrade(move |socket| serve_socket(s, socket, member))
}

async fn serve_socket<S: Channels>(s: S, socket: WebSocket, member: Member) {
    let cfg = s.hub().config();
    let pipe = WsPipe { socket };
    // WHY: open, say why, close: browsers hide an HTTP refusal's reason from JavaScript.
    if pipe.socket.protocol().and_then(|p| p.to_str().ok()) != Some(PROTOCOL) {
        let bye = ServerFrame::Bye {
            code: "protocol",
            reason: "subprotocol offrails.v1 required",
        };
        pipe.finish(Some(bye.frame()), close::PROTOCOL).await;
        return;
    }
    // SECURITY: per-person cap, checked and counted under one lock (test: the_connection_cap_refuses_the_next_socket_with_1008).
    let Some(guard) = s
        .hub()
        .try_register(&member, "websocket", cfg.max_conns_per_user)
    else {
        let bye = ServerFrame::Bye {
            code: "limit",
            reason: "too many connections",
        };
        pipe.finish(Some(bye.frame()), close::POLICY).await;
        return;
    };
    run(s, pipe, member, guard).await;
}

/// What a transport hands the connection loop.
#[derive(Debug)]
pub enum Inbound {
    Text(Frame),
    Binary,
    /// A pong or ping: the client is alive.
    Heard,
    /// The frame was over `max_frame`.
    TooBig,
    /// The client is gone.
    Closed,
}

/// A transport, as the connection loop sees it. `recv` must be cancel-safe.
pub trait Pipe: Send {
    /// WebSocket only: the server pings, and silence ends the connection.
    const HEARTBEATS: bool;
    fn recv(&mut self) -> impl Future<Output = Inbound> + Send;
    /// `false` when the client is gone or can't keep up.
    fn send(&mut self, frame: Frame) -> impl Future<Output = bool> + Send;
    fn ping(&mut self) -> impl Future<Output = bool> + Send;
    /// Sends `bye` (if any), then closes with `code`.
    fn finish(self, bye: Option<Frame>, code: u16) -> impl Future<Output = ()> + Send;
}

struct WsPipe {
    socket: WebSocket,
}

impl Pipe for WsPipe {
    const HEARTBEATS: bool = true;

    async fn recv(&mut self) -> Inbound {
        match self.socket.recv().await {
            Some(Ok(Message::Text(text))) => Inbound::Text(text),
            Some(Ok(Message::Binary(_))) => Inbound::Binary,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => Inbound::Heard,
            Some(Ok(Message::Close(_))) | None => Inbound::Closed,
            // tungstenite refuses a frame over max_frame_size/max_message_size while reading it.
            Some(Err(e)) if is_too_big(&e) => Inbound::TooBig,
            Some(Err(_)) => Inbound::Closed,
        }
    }

    async fn send(&mut self, frame: Frame) -> bool {
        self.socket.send(Message::Text(frame)).await.is_ok()
    }

    async fn ping(&mut self) -> bool {
        self.socket
            .send(Message::Ping(Default::default()))
            .await
            .is_ok()
    }

    async fn finish(mut self, bye: Option<Frame>, code: u16) {
        // WHY: bounded: a client that stopped reading must not hold the goodbye open either.
        let limit = Duration::from_secs(2);
        if let Some(bye) = bye {
            let _ = tokio::time::timeout(limit, self.socket.send(Message::Text(bye))).await;
        }
        let close = Message::Close(Some(CloseFrame {
            code,
            reason: "".into(),
        }));
        let _ = tokio::time::timeout(limit, self.socket.send(close)).await;
        // WHY: read until the client answers the close (or 2 s pass): dropping a socket with
        // unread data sends a TCP reset, which can overtake our `bye`.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(msg)) = self.socket.recv().await {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
            }
        })
        .await;
    }
}

fn is_too_big(e: &axum::Error) -> bool {
    let text = e.to_string();
    text.contains("too long") || text.contains("Space limit")
}

// ---- the connection loop ------------------------------------------------------------------------

/// A topic this connection joined. The connection task reads the receiver itself: a joined topic
/// costs a receiver and a flag, not a task and a channel.
struct Joined {
    rx: broadcast::Receiver<Outgoing>,
    /// Told it's falling behind (re-armed once it catches up).
    warned: bool,
}

type Topics = HashMap<Arc<str>, Joined>;

/// The next live frame of any joined topic; never resolves with no topics. Cancel-safe: a
/// receiver keeps its place, so losing a `select!` loses nothing.
async fn next_live(joined: &mut Topics) -> (Arc<str>, Result<Outgoing, RecvError>) {
    match joined.len() {
        0 => std::future::pending().await,
        1 => {
            let Some((topic, j)) = joined.iter_mut().next() else {
                return std::future::pending().await;
            };
            let got = j.rx.recv().await;
            (topic.clone(), got)
        }
        _ => {
            let mut any: FuturesUnordered<_> = joined
                .iter_mut()
                .map(|(topic, j)| async move { (topic.clone(), j.rx.recv().await) })
                .collect();
            match any.next().await {
                Some(next) => next,
                None => std::future::pending().await,
            }
        }
    }
}

/// A fixed window counter: `hit` says whether this hit is over `max` within `every`.
struct Window {
    start: Instant,
    hits: u32,
}

impl Window {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            hits: 0,
        }
    }

    fn over(&mut self, max: u32, every: Duration) -> bool {
        if self.start.elapsed() > every {
            self.start = Instant::now();
            self.hits = 0;
        }
        self.hits += 1;
        self.hits > max
    }
}

/// How a connection ends: `(bye code, reason, close code)`, or `None` when the client left.
type End = Option<(&'static str, &'static str, u16)>;

struct Conn<S: Channels> {
    s: S,
    hub: Hub<S::Room>,
    cfg: Arc<SocketConfig>,
    member: Member,
    info: Arc<ConnInfo>,
    id: u64,
    joined: Topics,
    frames: Window,
    joins: Window,
}

/// Runs one connection until it ends. `guard` counts it against the member's cap meanwhile.
pub async fn run<S: Channels, P: Pipe>(
    s: S,
    mut pipe: P,
    member: Member,
    guard: ConnGuard<S::Room>,
) {
    let hub = s.hub().clone();
    let cfg = s.hub().config();
    // INVARIANT: subscribed before the first await, so a logout in between isn't missed (test: logout_closes_that_sessions_sockets_only)
    let mut revoked = Box::pin(hub.revoked(&member.key));
    let mut closed = Box::pin(hub.closed());
    let mut resync = hub.resynced();
    resync.mark_unchanged();
    // The session was checked at the handshake; a logout between then and now sent its signal
    // before we listened, so check once more now that we do.
    if !Box::pin(s.session_valid(&member)).await {
        let bye = ServerFrame::Bye {
            code: "revoked",
            reason: "session ended",
        };
        pipe.finish(Some(bye.frame()), close::REVOKED).await;
        return;
    }

    let mut c = Conn {
        s,
        hub,
        cfg,
        member,
        info: guard.info().clone(),
        id: guard.id(),
        joined: HashMap::new(),
        frames: Window::new(),
        joins: Window::new(),
    };

    // WHY: one timer per connection: idle check, ping and revalidation run off the same tick.
    let mut last_heard = Instant::now();
    let mut last_ping = last_heard;
    let mut last_valid = last_heard;
    let tick_every = c
        .cfg
        .ping_every
        .min(c.cfg.revalidate_every)
        .min(c.cfg.idle_timeout / 2)
        .min(Duration::from_secs(5));
    let mut tick = tokio::time::interval(tick_every);
    tick.tick().await;

    let end: End = loop {
        tokio::select! {
            () = &mut closed => break Some(("restart", "server restarting", close::RESTART)),

            _ = resync.changed() => break Some(("restart", "reconnect to resume", close::RESTART)),

            () = &mut revoked => {
                // A session of this person may have ended: re-check our own.
                revoked = Box::pin(c.hub.revoked(&c.member.key));
                if !Box::pin(c.s.session_valid(&c.member)).await {
                    break Some(("revoked", "session ended", close::REVOKED));
                }
            }

            _ = tick.tick() => {
                let now = Instant::now();
                if P::HEARTBEATS && now.duration_since(last_heard) > c.cfg.idle_timeout {
                    break Some(("idle", "no activity", close::IDLE));
                }
                if due(now, last_valid, c.cfg.revalidate_every, tick_every) {
                    last_valid = now;
                    if !Box::pin(c.s.session_valid(&c.member)).await {
                        break Some(("revoked", "session expired", close::REVOKED));
                    }
                    for frame in Box::pin(c.reauthorize_all()).await {
                        if !c.send(&mut pipe, frame).await {
                            break;
                        }
                    }
                }
                if P::HEARTBEATS && due(now, last_ping, c.cfg.ping_every, tick_every) {
                    last_ping = now;
                    let pinged = tokio::time::timeout(c.cfg.send_timeout, pipe.ping()).await;
                    if !pinged.unwrap_or(false) {
                        break None;
                    }
                }
            }

            (topic, got) = next_live(&mut c.joined) => match got {
                Ok(out) => {
                    let frames = Box::pin(c.live(&topic, out)).await;
                    let mut gone = false;
                    for frame in frames {
                        if !c.send(&mut pipe, frame).await {
                            gone = true;
                            break;
                        }
                    }
                    if gone {
                        break None;
                    }
                }
                // SECURITY: a consumer past the topic's buffer is dropped, so one slow client can't make the server hold memory for everyone (test: a_slow_consumer_is_warned_then_dropped).
                Err(RecvError::Lagged(_)) => break Some(("too_slow", "connection too slow", close::TOO_SLOW)),
                Err(RecvError::Closed) => {
                    // The topic is gone: stop listening (a closed receiver would spin the loop).
                    c.leave(&topic);
                }
            },

            incoming = pipe.recv() => {
                last_heard = Instant::now();
                let text = match incoming {
                    Inbound::Text(text) => text,
                    Inbound::Heard => continue,
                    Inbound::Closed => break None,
                    // SECURITY: frames over 4 KiB close the connection (test: an_oversized_frame_closes_the_socket).
                    Inbound::TooBig => break Some(("too_big", "frame too large", close::TOO_BIG)),
                    Inbound::Binary => break Some(("protocol", "binary frames are not accepted", close::BINARY)),
                };
                c.hub.log_frame(&c.info, "in", &text);
                // SECURITY: more than max_in_per_10s frames closes the connection (test: flooding_closes_the_socket_with_1008).
                if c.frames.over(c.cfg.max_in_per_10s, Duration::from_secs(10)) {
                    break Some(("rate_limited", "too many messages", close::POLICY));
                }
                // Relays check the size before queuing; this covers any transport.
                if text.len() > c.cfg.max_frame {
                    break Some(("too_big", "frame too large", close::TOO_BIG));
                }
                let frame: ClientFrame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(_) => break Some(("protocol", "frames are JSON objects with a known t", close::PROTOCOL)),
                };
                let replies = Box::pin(c.handle(frame)).await;
                let Some(replies) = replies else {
                    break Some(("protocol", "malformed frame", close::PROTOCOL));
                };
                let mut gone = false;
                for frame in replies {
                    if !c.send(&mut pipe, frame).await {
                        gone = true;
                        break;
                    }
                }
                if gone {
                    break None;
                }
            }
        }
    };

    let bye = end.map(|(code, reason, close)| {
        let frame = ServerFrame::Bye { code, reason }.frame();
        c.hub.log_frame(&c.info, "out", &frame);
        (frame, close)
    });
    let topics: Vec<Arc<str>> = c.joined.keys().cloned().collect();
    for topic in topics {
        c.leave(&topic);
    }
    match bye {
        Some((frame, code)) => pipe.finish(Some(frame), code).await,
        None => pipe.finish(None, 1000).await,
    }
    drop(guard);
}

/// Has `every` elapsed since `last`, for work driven by a coarser `tick`? Half a tick early
/// counts, or a 20 s job on a 5 s tick would run every 25 s.
fn due(now: Instant, last: Instant, every: Duration, tick: Duration) -> bool {
    now.duration_since(last) + tick / 2 >= every
}

impl<S: Channels> Conn<S> {
    async fn send<P: Pipe>(&self, pipe: &mut P, frame: Frame) -> bool {
        self.hub.log_frame(&self.info, "out", &frame);
        // SECURITY: a client that stops reading can't park this task (and its revocation) on a full socket (test: a_client_that_stops_reading_is_dropped).
        tokio::time::timeout(self.cfg.send_timeout, pipe.send(frame))
            .await
            .unwrap_or(false)
    }

    fn leave(&mut self, topic: &str) {
        if self.joined.remove(topic).is_some() {
            self.hub.unsubscribe(topic, self.id);
            self.hub
                .set_topics(&self.info, self.joined.keys().map(|t| t.to_string()));
        }
    }

    /// One frame from a joined topic: what this connection sends for it.
    async fn live(&mut self, topic: &Arc<str>, out: Outgoing) -> Vec<Frame> {
        if out.reauthorize {
            return match self.s.authorize(&self.member, topic).await {
                Ok(()) => Vec::new(),
                Err(rejection) => {
                    self.leave(topic);
                    vec![err_frame(None, Some(topic), &rejection)]
                }
            };
        }
        if out.except == Some(self.id) {
            return Vec::new();
        }
        let mut frames = Vec::with_capacity(2);
        if let Some(j) = self.joined.get_mut(&**topic) {
            // Warn at 3/4 full; past the buffer the receiver lags and the connection closes.
            let queued = j.rx.len();
            if queued >= TOPIC_BUFFER * 3 / 4 && !j.warned {
                j.warned = true;
                frames.push(ServerFrame::Slow { topic }.frame());
            } else if queued < TOPIC_BUFFER / 2 {
                j.warned = false;
            }
        }
        let frame = match (&out.event, &out.data) {
            (Some(event), Some(data)) => {
                match <S::Room as Room>::handle_out(topic, &self.member, event, data) {
                    Delivery::Keep => Some(out.frame),
                    Delivery::Drop => None,
                    Delivery::Replace(d) => Some(event_frame(topic, out.seq, event, &d)),
                }
            }
            _ => Some(out.frame),
        };
        frames.extend(frame);
        frames
    }

    /// Re-checks every joined topic (at revalidation): a member removed since joining leaves.
    async fn reauthorize_all(&mut self) -> Vec<Frame> {
        let topics: Vec<Arc<str>> = self.joined.keys().cloned().collect();
        let mut frames = Vec::new();
        for topic in topics {
            if let Err(rejection) = self.s.authorize(&self.member, &topic).await {
                self.leave(&topic);
                frames.push(err_frame(None, Some(&topic), &rejection));
            }
        }
        frames
    }

    /// One client frame: the frames to write back, in order. `None` is a protocol error. A
    /// join's own frames (`ok`, the replay, `here`) go out before any live traffic: the topic's
    /// receiver is only read once these are written.
    async fn handle(&mut self, frame: ClientFrame) -> Option<Vec<Frame>> {
        Some(match frame {
            ClientFrame::Ping { reference } => {
                if !plain_ref(&reference) {
                    return None;
                }
                vec![
                    ServerFrame::Pong {
                        reference: reference.as_ref(),
                    }
                    .frame(),
                ]
            }

            ClientFrame::Leave { reference, topic } => {
                if !plain_ref(&reference) {
                    return None;
                }
                if self.joined.contains_key(topic.as_str()) {
                    self.leave(&topic);
                    vec![
                        ServerFrame::Ok {
                            reference: reference.as_ref(),
                            topic: Some(&topic),
                            data: &Payload::Null,
                            replay: None,
                            gap: None,
                        }
                        .frame(),
                    ]
                } else {
                    vec![err_frame(
                        reference.as_ref(),
                        Some(&topic),
                        &Rejection::not_joined(),
                    )]
                }
            }

            ClientFrame::Join {
                reference,
                topic,
                since,
            } => {
                if !plain_ref(&reference) {
                    return None;
                }
                let r = reference.as_ref();
                if !plain_topic(&topic) {
                    return Some(vec![err_frame(r, None, &Rejection::unknown_topic())]);
                }
                if self.joined.contains_key(topic.as_str()) {
                    let rejection = Rejection::new("already_joined", "Already joined.");
                    return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                }
                if self.joined.len() >= self.cfg.max_topics {
                    let rejection = Rejection::new("limit", "Too many topics on one connection.");
                    return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                }
                if self
                    .joins
                    .over(self.cfg.max_joins_per_min, Duration::from_secs(60))
                {
                    let rejection = Rejection::rate_limited("Joining too fast.");
                    return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                }
                // SECURITY: every join is authorized by the app against the database now; deny by default (test: joining_someone_elses_topic_is_forbidden).
                if let Err(rejection) = self.s.authorize(&self.member, &topic).await {
                    return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                }
                // INVARIANT: subscribe before computing the reply, so nothing broadcast in between is lost (test: a_reconnect_replays_exactly_what_was_missed)
                let rx = self.hub.subscribe(&topic, self.id);
                let reply = match self.hub.join(&self.s, &topic, &self.member, since).await {
                    Ok(reply) => reply,
                    Err(rejection) => {
                        self.hub.unsubscribe(&topic, self.id);
                        return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                    }
                };
                // Track, then the snapshot: `here` includes the joiner.
                self.hub.track(&topic, self.id, &self.member);
                let mut frames = Vec::with_capacity(reply.replay.len() + 2);
                frames.push(
                    ServerFrame::Ok {
                        reference: r,
                        topic: Some(&topic),
                        data: &reply.data,
                        replay: Some(reply.replay.len()),
                        gap: Some(reply.gap),
                    }
                    .frame(),
                );
                frames.extend(reply.replay);
                frames.push(presence_here_frame(&topic, &self.hub.presence_list(&topic)));
                self.joined
                    .insert(Arc::from(topic.as_str()), Joined { rx, warned: false });
                self.hub
                    .set_topics(&self.info, self.joined.keys().map(|t| t.to_string()));
                frames
            }

            ClientFrame::Push {
                reference,
                topic,
                event,
                data,
            } => {
                if !plain_ref(&reference) {
                    return None;
                }
                let r = reference.as_ref();
                if !self.joined.contains_key(topic.as_str()) {
                    return Some(vec![err_frame(r, Some(&topic), &Rejection::not_joined())]);
                }
                // SECURITY: every push is re-authorized: a member removed since joining is refused and leaves (test: a_push_after_losing_access_is_forbidden_and_leaves).
                if let Err(rejection) = self.s.authorize(&self.member, &topic).await {
                    self.leave(&topic);
                    return Some(vec![err_frame(r, Some(&topic), &rejection)]);
                }
                if event.len() > 64 {
                    return Some(vec![err_frame(r, Some(&topic), &Rejection::bad_event())]);
                }
                let result = self
                    .hub
                    .handle_in(&self.s, &topic, &self.member, self.id, &event, data)
                    .await;
                match result {
                    Ok(data) => vec![
                        ServerFrame::Ok {
                            reference: r,
                            topic: Some(&topic),
                            data: &data,
                            replay: None,
                            gap: None,
                        }
                        .frame(),
                    ],
                    Err(rejection) => vec![err_frame(r, Some(&topic), &rejection)],
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn cfg() -> SocketConfig {
        let mut c = SocketConfig::new(vec!["http://localhost:3000".into()]);
        c.token_origins = vec!["https://partner.example.com".into()];
        c
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn origin_of_drops_the_path() {
        assert_eq!(
            origin_of("https://a.example.com/x?y"),
            "https://a.example.com"
        );
        assert_eq!(origin_of("http://localhost:3000"), "http://localhost:3000");
    }

    #[test]
    fn the_handshake_origin_rule() {
        let c = cfg();
        let same = headers(&[("origin", "http://localhost:3000")]);
        let evil = headers(&[("origin", "https://evil.example")]);
        let partner = headers(&[("origin", "https://partner.example.com")]);
        let none = HeaderMap::new();
        // Cookies: exact origin only, and a missing one is refused.
        assert!(origin_allowed(&c, &same, false));
        assert!(!origin_allowed(&c, &evil, false));
        assert!(!origin_allowed(&c, &partner, false));
        assert!(!origin_allowed(&c, &none, false));
        // Tokens: no ambient credential, so no Origin (a native client) or an allowed one.
        assert!(origin_allowed(&c, &same, true));
        assert!(origin_allowed(&c, &partner, true));
        assert!(origin_allowed(&c, &none, true));
        assert!(!origin_allowed(&c, &evil, true));
    }

    #[test]
    fn cross_site_gets_are_recognised() {
        let c = cfg();
        assert!(!is_cross_site(&c, &HeaderMap::new()));
        assert!(!is_cross_site(
            &c,
            &headers(&[("sec-fetch-site", "same-origin")])
        ));
        assert!(is_cross_site(
            &c,
            &headers(&[("sec-fetch-site", "cross-site")])
        ));
        assert!(is_cross_site(
            &c,
            &headers(&[("sec-fetch-site", "same-site")])
        ));
        assert!(is_cross_site(
            &c,
            &headers(&[("origin", "https://evil.example")])
        ));
        assert!(!is_cross_site(
            &c,
            &headers(&[("origin", "http://localhost:3000")])
        ));
    }

    #[test]
    fn client_frames_are_typed_and_tolerant_of_new_fields() {
        let f: ClientFrame =
            serde_json::from_str(r#"{"t":"join","ref":"1","topic":"room:a","since":4,"x":1}"#)
                .unwrap();
        assert!(matches!(f, ClientFrame::Join { since: Some(4), .. }));
        assert!(serde_json::from_str::<ClientFrame>(r#"{"t":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ClientFrame>(r#"["join"]"#).is_err());
        let push: ClientFrame =
            serde_json::from_str(r#"{"t":"push","ref":2,"topic":"room:a","event":"post"}"#)
                .unwrap();
        assert!(
            matches!(push, ClientFrame::Push { data, reference: Some(Ref::Num(2)), .. } if data.is_null())
        );
    }

    #[test]
    fn topics_and_refs_stay_short_and_plain() {
        assert!(plain_topic("room:lobby") && plain_topic("user:12"));
        assert!(!plain_topic("") && !plain_topic("room lobby") && !plain_topic("<b>"));
        assert!(!plain_topic(&"a".repeat(129)));
        assert!(plain_ref(&Some(Ref::Str("x".repeat(64)))));
        assert!(!plain_ref(&Some(Ref::Str("x".repeat(65)))));
    }

    #[test]
    fn server_frames_serialize_as_the_protocol_says() {
        let r = Ref::Str("7".into());
        let ok = ServerFrame::Ok {
            reference: Some(&r),
            topic: Some("room:a"),
            data: &serde_json::json!({}),
            replay: Some(2),
            gap: Some(false),
        }
        .frame();
        assert_eq!(
            ok.as_str(),
            r#"{"t":"ok","ref":"7","topic":"room:a","data":{},"replay":2,"gap":false}"#
        );
        let bye = ServerFrame::Bye {
            code: "revoked",
            reason: "session ended",
        }
        .frame();
        assert_eq!(
            bye.as_str(),
            r#"{"t":"bye","code":"revoked","reason":"session ended"}"#
        );
    }

    #[test]
    fn periodic_work_on_a_coarse_tick_keeps_its_cadence() {
        let tick = Duration::from_secs(5);
        let every = Duration::from_secs(20);
        let start = Instant::now();
        let (mut last, mut ran) = (start, vec![]);
        for k in 1..=20u64 {
            let now = start + tick * k as u32 + Duration::from_millis(1 + k % 3);
            if due(now, last, every, tick) {
                last = now;
                ran.push(k * 5);
            }
        }
        assert_eq!(ran, vec![20, 40, 60, 80, 100]);
    }

    fn size_of_future<F: Future>(_: impl FnOnce() -> F) -> usize {
        std::mem::size_of::<F>()
    }

    /// A connection's task lives as long as the socket: with 10,000 sockets every byte here is
    /// 10 KB. Awaited database work inside the loop is boxed, not part of this future.
    #[test]
    #[allow(unreachable_code, clippy::diverging_sub_expression)]
    fn an_idle_connection_future_stays_small() {
        let n = size_of_future(|| {
            run::<crate::state::AppState, WsPipe>(todo!(), todo!(), todo!(), todo!())
        });
        assert!(n <= 4 * 1024, "the connection future grew to {n} bytes");
    }
}
