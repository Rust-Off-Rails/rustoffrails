//! Test helpers: an `offrails.v1` WebSocket client (tokio-tungstenite), the app on a real port,
//! signed-in guests, and an in-process reader for the SSE and long-poll relays.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use offrails_host::config::Config;
use offrails_host::routes;
use offrails_host::state::AppState;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tower::ServiceExt;

use super::people;

/// APP_URL's origin in `Config::test()`.
pub const ORIGIN: &str = "http://localhost:3000";
/// How long a test waits for a frame that should come.
pub const WAIT: Duration = Duration::from_secs(5);

/// Serves `state`'s router on 127.0.0.1 (sockets need a real port). Abort the handle to stop it.
pub async fn serve(state: AppState) -> (SocketAddr, JoinHandle<()>) {
    let router = routes::router(state).expect("router");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let service = router.into_make_service_with_connect_info::<SocketAddr>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, service).await;
    });
    (addr, handle)
}

/// A signed-in person (a guest, or an account with auth-core): what a browser holds.
#[derive(Clone, Debug)]
pub struct Guest {
    pub id: i64,
    pub name: String,
    pub session_id: i64,
    pub token: String,
}

impl Guest {
    pub fn cookie(&self) -> String {
        format!("session_token={}", self.token)
    }

    pub fn key(&self) -> String {
        self.id.to_string()
    }

    /// Another browser of the same person: a second session.
    pub async fn second_session(&self, state: &AppState) -> Guest {
        let (session_id, token) = people::start_session(state, self.id).await;
        Guest {
            session_id,
            token,
            ..self.clone()
        }
    }
}

/// A signed-in person named `name` (a development guest, or an account with auth-core).
pub async fn guest(state: &AppState, name: &str) -> Guest {
    let (id, name, session_id, token) = people::create(state, name).await;
    Guest {
        id,
        name,
        session_id,
        token,
    }
}

/// A refusal (`err` frame).
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct JoinedTopic {
    pub data: Value,
    /// The replayed `event` frames.
    pub replay: Vec<Value>,
    pub gap: bool,
}

pub struct Ws {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    buffered: VecDeque<Value>,
    next_ref: u64,
    pub protocol: Option<String>,
}

impl Ws {
    /// Offers `offrails.v1`, with the cookie and Origin of a browser on APP_URL.
    pub async fn as_guest(addr: SocketAddr, g: &Guest) -> Ws {
        Self::connect(addr, Some(&g.cookie()), Some(ORIGIN))
            .await
            .expect("handshake")
    }

    pub async fn connect(
        addr: SocketAddr,
        cookie: Option<&str>,
        origin: Option<&str>,
    ) -> Result<Ws, StatusCode> {
        Self::connect_with(addr, cookie, origin, &["offrails.v1"]).await
    }

    /// Any subprotocols (none, others, a token): the handshake's status on refusal.
    pub async fn connect_with(
        addr: SocketAddr,
        cookie: Option<&str>,
        origin: Option<&str>,
        protocols: &[&str],
    ) -> Result<Ws, StatusCode> {
        let mut req = format!("ws://{addr}/socket")
            .into_client_request()
            .expect("request");
        let h = req.headers_mut();
        if let Some(c) = cookie {
            h.insert("cookie", c.parse().expect("cookie"));
        }
        if let Some(o) = origin {
            h.insert("origin", o.parse().expect("origin"));
        }
        if !protocols.is_empty() {
            h.insert(
                "sec-websocket-protocol",
                protocols.join(", ").parse().expect("protocols"),
            );
        }
        match connect_async(req).await {
            Ok((stream, response)) => Ok(Ws {
                protocol: response
                    .headers()
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                stream,
                buffered: VecDeque::new(),
                next_ref: 0,
            }),
            Err(tungstenite::Error::Http(response)) => {
                Err(StatusCode::from_u16(response.status().as_u16()).expect("status"))
            }
            // The upgrade answered, but the client refused it (no subprotocol it offered).
            Err(tungstenite::Error::Protocol(_)) => Err(StatusCode::SWITCHING_PROTOCOLS),
            Err(e) => panic!("handshake failed: {e}"),
        }
    }

    pub async fn send_text(&mut self, text: &str) {
        self.stream
            .send(Message::Text(text.into()))
            .await
            .expect("send");
    }

    pub async fn send_json(&mut self, v: Value) {
        self.send_text(&v.to_string()).await;
    }

    pub async fn send_binary(&mut self, bytes: Vec<u8>) {
        let _ = self.stream.send(Message::Binary(bytes.into())).await;
    }

    /// The next frame from the socket (not the buffer), or `None` on close or after `wait`.
    async fn read(&mut self, wait: Duration) -> Option<Value> {
        loop {
            match tokio::time::timeout(wait, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    return Some(serde_json::from_str(&t).expect("server frames are JSON"));
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => continue,
                Ok(Some(Ok(Message::Binary(_)))) => panic!("the server never sends binary"),
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    return None;
                }
            }
        }
    }

    /// The next frame, buffered ones first.
    pub async fn next(&mut self) -> Option<Value> {
        if let Some(v) = self.buffered.pop_front() {
            return Some(v);
        }
        self.read(WAIT).await
    }

    /// The first frame matching `pred`; others are kept for later.
    pub async fn recv_where(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.buffered.iter().position(&pred) {
            return self.buffered.remove(i).expect("index");
        }
        loop {
            let Some(v) = self.read(WAIT).await else {
                panic!("no frame matching {what}; kept: {:?}", self.buffered);
            };
            if pred(&v) {
                return v;
            }
            self.buffered.push_back(v);
        }
    }

    /// True when no frame matching `pred` arrives within `ms` (others are kept).
    pub async fn quiet(&mut self, ms: u64, pred: impl Fn(&Value) -> bool) -> bool {
        if self.buffered.iter().any(&pred) {
            return false;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.read(left).await {
                Some(v) if pred(&v) => return false,
                Some(v) => self.buffered.push_back(v),
                None => return true,
            }
        }
    }

    fn reference(&mut self) -> String {
        self.next_ref += 1;
        self.next_ref.to_string()
    }

    /// Sends `frame` with a fresh `ref`; returns the `ok` or `err` carrying it.
    pub async fn request(&mut self, mut frame: Value) -> Value {
        let r = self.reference();
        frame["ref"] = json!(r);
        self.send_json(frame).await;
        self.recv_where("the reply", |v| {
            v["ref"] == json!(r) && matches!(v["t"].as_str(), Some("ok" | "err" | "pong"))
        })
        .await
    }

    fn result(reply: Value) -> Result<Value, Rejected> {
        if reply["t"] == "err" {
            Err(Rejected {
                code: reply["code"].as_str().unwrap_or_default().into(),
                message: reply["message"].as_str().unwrap_or_default().into(),
            })
        } else {
            Ok(reply)
        }
    }

    /// Joins; on success collects exactly `replay` replayed events (they follow the `ok`).
    pub async fn join(&mut self, topic: &str, since: Option<i64>) -> Result<JoinedTopic, Rejected> {
        let mut frame = json!({ "t": "join", "topic": topic });
        if let Some(s) = since {
            frame["since"] = json!(s);
        }
        let ok = Self::result(self.request(frame).await)?;
        let n = ok["replay"].as_u64().unwrap_or(0);
        let mut replay = Vec::new();
        for _ in 0..n {
            replay.push(
                self.recv_where("a replayed event", |v| {
                    v["t"] == "event" && v["topic"] == topic && v.get("seq").is_some()
                })
                .await,
            );
        }
        Ok(JoinedTopic {
            data: ok["data"].clone(),
            replay,
            gap: ok["gap"].as_bool().unwrap_or(false),
        })
    }

    pub async fn push(&mut self, topic: &str, event: &str, data: Value) -> Result<Value, Rejected> {
        let reply = self
            .request(json!({ "t": "push", "topic": topic, "event": event, "data": data }))
            .await;
        Self::result(reply).map(|ok| ok["data"].clone())
    }

    pub async fn leave(&mut self, topic: &str) -> Result<Value, Rejected> {
        let reply = self.request(json!({ "t": "leave", "topic": topic })).await;
        Self::result(reply)
    }

    /// The next `event` named `event` on `topic`.
    pub async fn event(&mut self, topic: &str, event: &str) -> Value {
        self.recv_where(&format!("{event} on {topic}"), |v| {
            v["t"] == "event" && v["topic"] == topic && v["event"] == event
        })
        .await
    }

    /// The next `presence` frame on `topic`.
    pub async fn presence(&mut self, topic: &str) -> Value {
        self.recv_where("presence", |v| v["t"] == "presence" && v["topic"] == topic)
            .await
    }

    /// Reads until the server closes: `(close code, bye code)`.
    pub async fn closed(&mut self) -> (u16, Option<String>) {
        let mut bye = self
            .buffered
            .iter()
            .find(|v| v["t"] == "bye")
            .and_then(|v| v["code"].as_str().map(str::to_owned));
        loop {
            match tokio::time::timeout(WAIT, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(&t).expect("json");
                    if v["t"] == "bye" {
                        bye = v["code"].as_str().map(str::to_owned);
                    }
                }
                Ok(Some(Ok(Message::Close(frame)))) => {
                    return (frame.map_or(1005, |f| u16::from(f.code)), bye);
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) | Ok(None) => return (1006, bye),
                Err(_) => panic!("the server didn't close the socket (bye so far: {bye:?})"),
            }
        }
    }
}

// ---- the HTTP relays, in process ----------------------------------------------------------------

/// An SSE or long-poll relay driven through the router, as a browser would.
pub struct Relay {
    router: Router,
    pub cookie: String,
    pub conn: u64,
    pub token: String,
    pub kind: &'static str,
    body: Option<Body>,
    pending: String,
    pub frames: VecDeque<Value>,
}

pub fn router_of(state: &AppState) -> Router {
    routes::router(state.clone()).expect("router")
}

/// A GET or POST with a browser's same-origin headers.
pub fn browser_request(
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<String>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", cookie)
        .header("sec-fetch-site", "same-origin")
        .header("origin", ORIGIN);
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    b.body(body.map_or_else(Body::empty, Body::from))
        .expect("request")
}

impl Relay {
    /// `GET /socket/sse`: reads `hello`, keeps the stream open.
    pub async fn sse(state: &AppState, g: &Guest) -> Relay {
        let router = router_of(state);
        let res = router
            .clone()
            .oneshot(browser_request("GET", "/socket/sse", &g.cookie(), None))
            .await
            .expect("infallible");
        assert_eq!(res.status(), StatusCode::OK, "opening the SSE relay");
        let mut relay = Relay {
            router,
            cookie: g.cookie(),
            conn: 0,
            token: String::new(),
            kind: "sse",
            body: Some(res.into_body()),
            pending: String::new(),
            frames: VecDeque::new(),
        };
        let (event, data) = relay.next_sse().await.expect("hello");
        assert_eq!(event, "hello");
        let hello: Value = serde_json::from_str(&data).expect("json");
        relay.conn = hello["conn"].as_u64().expect("conn");
        relay.token = hello["token"].as_str().expect("token").into();
        relay
    }

    /// `POST /socket/poll`.
    pub async fn longpoll(state: &AppState, g: &Guest) -> Relay {
        let router = router_of(state);
        let res = router
            .clone()
            .oneshot(browser_request("POST", "/socket/poll", &g.cookie(), None))
            .await
            .expect("infallible");
        assert_eq!(res.status(), StatusCode::OK, "opening the long-poll relay");
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let hello: Value = serde_json::from_slice(&bytes).expect("json");
        Relay {
            router,
            cookie: g.cookie(),
            conn: hello["conn"].as_u64().expect("conn"),
            token: hello["token"].as_str().expect("token").into(),
            kind: "poll",
            body: None,
            pending: String::new(),
            frames: VecDeque::new(),
        }
    }

    /// The next SSE event `(name, data)`, skipping comments; `None` when the stream ends.
    async fn next_sse(&mut self) -> Option<(String, String)> {
        loop {
            if let Some(end) = self.pending.find("\n\n") {
                let block: String = self.pending.drain(..end + 2).collect();
                let mut event = String::from("message");
                let mut data = Vec::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_owned();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push(v.strip_prefix(' ').unwrap_or(v).to_owned());
                    }
                }
                if data.is_empty() {
                    continue;
                }
                return Some((event, data.join("\n")));
            }
            let body = self.body.as_mut()?;
            match tokio::time::timeout(WAIT, body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.pending.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                Ok(_) => {
                    self.body = None;
                    return None;
                }
                Err(_) => return None,
            }
        }
    }

    /// The path frames are posted to.
    pub fn send_path(&self) -> String {
        match self.kind {
            "sse" => format!("/socket/sse/{}", self.conn),
            _ => format!("/socket/poll/{}", self.conn),
        }
    }

    /// Posts one frame with this relay's token (or `token`): the status.
    pub async fn post_as(&self, cookie: &str, token: &str, text: &str) -> StatusCode {
        let mut req = browser_request("POST", &self.send_path(), cookie, Some(text.to_owned()));
        req.headers_mut()
            .insert("x-offrails-token", token.parse().expect("token"));
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("infallible")
            .status()
    }

    pub async fn post(&self, frame: Value) -> StatusCode {
        self.post_as(&self.cookie, &self.token, &frame.to_string())
            .await
    }

    /// One long-poll GET with `token`: status and frames.
    pub async fn poll_as(&self, cookie: &str, token: &str) -> (StatusCode, Vec<Value>) {
        let mut req = browser_request("GET", &format!("/socket/poll/{}", self.conn), cookie, None);
        req.headers_mut()
            .insert("x-offrails-token", token.parse().expect("token"));
        let res = self.router.clone().oneshot(req).await.expect("infallible");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let frames = if status == StatusCode::OK {
            serde_json::from_slice(&bytes).expect("a JSON array")
        } else {
            Vec::new()
        };
        (status, frames)
    }

    /// The next frame down the relay (`None` when it ended).
    pub async fn next(&mut self) -> Option<Value> {
        if let Some(v) = self.frames.pop_front() {
            return Some(v);
        }
        match self.kind {
            "sse" => loop {
                let (event, data) = self.next_sse().await?;
                if event == "frame" {
                    return Some(serde_json::from_str(&data).expect("json"));
                }
            },
            _ => {
                let (cookie, token) = (self.cookie.clone(), self.token.clone());
                let (status, frames) = self.poll_as(&cookie, &token).await;
                if status != StatusCode::OK {
                    return None;
                }
                self.frames.extend(frames);
                self.frames.pop_front()
            }
        }
    }

    pub async fn recv_where(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            match self.next().await {
                Some(v) if pred(&v) => return v,
                Some(_) => continue,
                None => panic!("the relay ended before {what}"),
            }
        }
    }
}

/// `Config::test()` with a shorter session re-check.
pub fn config_revalidating_every(secs: u64) -> Config {
    let mut c = Config::test();
    c.realtime.revalidate = Duration::from_secs(secs);
    c
}
