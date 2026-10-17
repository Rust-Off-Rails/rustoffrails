//! Channels: the app's side of realtime (Action Cable channels / Phoenix channels).
//!
//! GUIDE: .offrails/guides/channels.md#the-app-side
//!
//! - **Topics** (`authorize`): `room:<slug>` (a live room, `live_room.rs`) and `user:<id>` (a
//!   person's private notices). Anything else is refused: deny by default.
//! - **Transports**: `GET /socket` (WebSocket, `offrails.v1`), then `/socket/sse` and
//!   `/socket/poll` when a proxy breaks WebSockets. `POST /socket/token` mints a socket token for
//!   clients that can't send the session cookie.
//! - **Server API**: `s.hub.broadcast(topic, seq, event, data)`, `broadcast_from`, `emit`,
//!   `reauthorize`, `revoke` (`session_ended` below), and `refresh` for htmx regions.

pub mod identity;
pub mod live_room;
// samples: channel mods

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use sea_orm::DatabaseConnection;
use serde_json::json;

use crate::config::Config;
use crate::controllers::application::ClientIp;
use crate::error::AppError;
use crate::kernel::channels::{self as kchannels, Channels, SocketConfig};
use crate::kernel::cluster::{self, Cluster};
use crate::kernel::hub::{Hub, Member, Rejection};
use crate::kernel::{socket_token, transports};
use crate::state::AppState;

pub use live_room::AppRoom;

use identity::{SocketPerson, member_for};

/// The routes. Their `ROUTES` rows are `Access::User`.
pub fn router() -> Router<AppState> {
    // WHY: relay bodies are single frames: refuse more than a frame (plus slack) before reading.
    let frame_limit = DefaultBodyLimit::max(8 * 1024);
    Router::new()
        .route("/socket", get(socket))
        .route("/socket/sse", get(socket_sse))
        .route("/socket/sse/{id}", post(socket_send).layer(frame_limit))
        .route("/socket/poll", post(socket_poll_open))
        // WHY: explicit GET and POST on one path: a POST to a GET-only route would answer 405.
        .route(
            "/socket/poll/{id}",
            get(socket_poll).post(socket_send).layer(frame_limit),
        )
        .route("/socket/token", post(socket_token))
}

/// The hub for this process (`AppState::new`).
pub fn build_hub(config: &Config, db: &DatabaseConnection) -> anyhow::Result<Hub<AppRoom>> {
    let mut origins = vec![kchannels::origin_of(&config.app_url)];
    if let Some(port) = config.dev_http_port {
        origins.push(format!("http://localhost:{port}"));
    }
    let mut socket = SocketConfig::new(origins);
    socket.token_origins = config.realtime.token_origins.clone();
    socket.transports = config.realtime.transports.clone();
    socket.revalidate_every = config.realtime.revalidate;
    let node = cluster::node_name(config.realtime.node.as_deref());
    let cluster = if config.realtime.cluster {
        Cluster::postgres(node, db.clone())?
    } else {
        Cluster::off(node)
    };
    let hub = Hub::with_cluster(cluster, socket);
    // SECURITY: frame text is kept for /dev/sockets in development only; production keeps no message text (test: the_frame_log_is_off_outside_development).
    hub.log_frames(config.is_development());
    Ok(hub)
}

impl Channels for AppState {
    type Room = AppRoom;

    fn hub(&self) -> &Hub<AppRoom> {
        &self.hub
    }

    /// APP: who may join which topic, against the database now. Called on every join, every
    /// push, every revalidation and on `Hub::reauthorize`. Add an arm per kind of topic;
    /// everything else stays refused.
    async fn authorize(&self, member: &Member, topic: &str) -> Result<(), Rejection> {
        match topic.split_once(':') {
            Some(("room", slug)) => live_room::authorize(self, member, slug).await,
            // SECURITY: a private topic accepts only its owner, by exact id (test: joining_someone_elses_topic_is_forbidden).
            Some(("user", id)) if id == member.key => Ok(()),
            Some(("user", _)) => Err(Rejection::forbidden()),
            _ => Err(Rejection::unknown_topic()),
        }
    }

    async fn session_valid(&self, member: &Member) -> bool {
        identity::session_live(self, member.session).await
    }
}

/// A session of `person_id` ended (sign out, password change, revocation): their connections on
/// every node re-check their own session, and the ended one closes with `bye revoked` (4401).
pub fn session_ended(s: &AppState, person_id: i64) {
    s.hub.revoke(&person_id.to_string());
}

/// htmx page refreshes over a channel: every page on `topic` re-fetches its
/// `[data-live-region="<region>"]` (its `data-src`, through the normal authorized GET). Send it
/// after the commit. For rare changes (a headline, settings); a frequent one is an `event`.
pub fn refresh(s: &AppState, topic: &str, region: &str) {
    s.hub
        .broadcast(topic, None, "refresh", &json!({ "region": region }));
}

fn offered(s: &AppState, transport: &str) -> bool {
    s.hub.config().offers(transport)
}

/// `GET /socket`: the WebSocket (`offrails.v1`). With the session cookie from APP_URL's origin,
/// or with a socket token (`offrails.token.<token>` subprotocol) from anywhere allowed.
pub async fn socket(
    State(s): State<AppState>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, AppError> {
    if !offered(&s, "websocket") {
        return Err(AppError::NotFound);
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return Ok(rejection.into_response()),
    };
    let token = kchannels::offered_token(&ws);
    // SECURITY: cross-site WebSocket hijacking: a cookie handshake needs APP_URL's exact Origin, a missing one included (test: a_foreign_or_missing_origin_is_refused_at_the_handshake).
    if !kchannels::origin_allowed(&s.hub.config(), &headers, token.is_some()) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let signed = match token {
        Some(token) => redeem_token(&s, &token).await?,
        None => identity::current(&s, &headers).await?,
    };
    // A bare 401: a WebSocket can't follow a redirect to the sign-in page.
    let Some(signed) = signed else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    Ok(kchannels::upgrade(s, ws, member_for(&signed)))
}

/// A socket token → the person, if it is genuine, fresh, unused, and its session is live.
async fn redeem_token(s: &AppState, token: &str) -> Result<Option<identity::Signed>, AppError> {
    let Some(secret) = s.config.realtime.secret.as_deref() else {
        return Ok(None);
    };
    let now = Utc::now();
    let Ok(claims) = socket_token::verify(secret.as_bytes(), token, now) else {
        return Ok(None);
    };
    if socket_token::redeem(&s.db, &claims, now).await.is_err() {
        return Ok(None);
    }
    let signed = identity::by_session(s, claims.session_id).await?;
    // The session must still belong to the person the token names.
    Ok(signed.filter(|signed| signed.person.id == claims.person_id))
}

/// `POST /socket/token`: `{token, expires_in}` for this session, single use, 60 s.
pub async fn socket_token(
    State(s): State<AppState>,
    SocketPerson(signed): SocketPerson,
    ip: ClientIp,
) -> Result<Response, AppError> {
    let Some(secret) = s.config.realtime.secret.as_deref() else {
        return Err(AppError::NotFound);
    };
    s.rate_limit
        .check(
            &ip.key(&format!("socket_token:{}", signed.session_id)),
            10,
            std::time::Duration::from_secs(60),
        )
        .await?;
    let token = socket_token::mint(
        secret.as_bytes(),
        signed.person.id,
        signed.session_id,
        Utc::now(),
    );
    Ok(Json(json!({
        "token": token,
        "expires_in": socket_token::TTL.num_seconds(),
        "protocol": format!("{}{token}", kchannels::TOKEN_PROTOCOL_PREFIX),
    }))
    .into_response())
}

/// `GET /socket/sse`: the same protocol over Server-Sent Events (frames go up by POST).
pub async fn socket_sse(
    State(s): State<AppState>,
    SocketPerson(signed): SocketPerson,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !offered(&s, "sse") {
        return Err(AppError::NotFound);
    }
    Ok(transports::sse_open(s, &headers, member_for(&signed)))
}

/// `POST /socket/poll`: starts a long-polling relay.
pub async fn socket_poll_open(
    State(s): State<AppState>,
    SocketPerson(signed): SocketPerson,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !offered(&s, "longpoll") {
        return Err(AppError::NotFound);
    }
    Ok(transports::poll_open(s, &headers, member_for(&signed)))
}

/// `GET /socket/poll/{id}`: waits up to 25 s; the frames that arrived, as a JSON array.
pub async fn socket_poll(
    State(s): State<AppState>,
    SocketPerson(signed): SocketPerson,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !offered(&s, "longpoll") {
        return Err(AppError::NotFound);
    }
    Ok(transports::poll_get(s, &headers, member_for(&signed), id).await)
}

/// `POST /socket/sse/{id}` and `POST /socket/poll/{id}`: one client frame. 202.
pub async fn socket_send(
    State(s): State<AppState>,
    SocketPerson(signed): SocketPerson,
    Path(id): Path<u64>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, AppError> {
    Ok(transports::relay_post(s, &headers, member_for(&signed), id, body).await)
}

/// The hub's limits, for pages that tell the client what the server offers.
pub fn transports_offered(s: &AppState) -> Arc<SocketConfig> {
    s.hub.config()
}
