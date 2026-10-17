//! `/dev/sockets` (LiveDashboard's sockets page): this node, the cluster, every topic with who is
//! present, every connection with its transport and counters, and a live decoded frame log.
//! Development only (merged in `routes.rs` under `is_development()`), every response `no-store`.
//!
//! GUIDE: .offrails/guides/channels.md#devsockets

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::kernel::flash::RequestFlash;
use crate::kernel::hub::{ConnSnapshot, Snapshot};
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui::{self, Chrome};
use crate::state::AppState;

/// How often the page refreshes its live part. APP: slower on a busy node.
pub const REFRESH: &str = "every 2s";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dev/sockets", get(index))
        .route("/dev/sockets/live", get(live))
        .route("/dev/sockets.json", get(json))
        // SECURITY: frames and presence never land in a shared or back/forward cache (test: every_sockets_response_is_no_store).
        .layer(ui::no_store())
}

#[derive(Debug, Default, Deserialize)]
pub struct SocketsQuery {
    /// The connection whose frame log is shown.
    conn: Option<u64>,
}

/// What both the page and its live part show.
pub struct Live {
    pub snap: Snapshot,
    /// The selected connection (its frame log), if it is still open.
    pub selected: Option<ConnSnapshot>,
    pub refresh: &'static str,
    pub live_url: String,
}

impl Live {
    fn new(state: &AppState, conn: Option<u64>) -> Self {
        let snap = state.hub.snapshot();
        let selected = conn.and_then(|id| snap.conns.iter().find(|c| c.id == id).cloned());
        let live_url = match conn {
            Some(id) => format!("/dev/sockets/live?conn={id}"),
            None => "/dev/sockets/live".to_owned(),
        };
        Self {
            snap,
            selected,
            refresh: REFRESH,
            live_url,
        }
    }

    pub fn rate(per_sec: &f64) -> String {
        format!("{per_sec:.1}/s")
    }

    pub fn age(secs: &i64) -> String {
        ui::duration(secs * 1000)
    }
}

#[derive(Template)]
#[template(path = "kernel/sockets/index.html")]
struct SocketsIndex {
    chrome: Chrome,
    live: Live,
    transports: String,
    cluster: &'static str,
}

#[derive(Template)]
#[template(path = "kernel/sockets/_live.html")]
struct SocketsLive {
    live: Live,
}

async fn index(
    State(state): State<AppState>,
    Query(q): Query<SocketsQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Response {
    let chrome = Chrome::new(
        "Sockets",
        "sockets",
        state.assets.assets(),
        true,
        flash.and_then(|f| f.0.0),
        theme,
    )
    .stylesheet(state.assets.path("kernel/sockets.css"));
    ui::page(SocketsIndex {
        chrome,
        live: Live::new(&state, q.conn),
        transports: state.hub.config().transports.join(", "),
        cluster: state.hub.cluster().kind(),
    })
}

/// `GET /dev/sockets/live`: the part the page swaps in every 2 s.
async fn live(State(state): State<AppState>, Query(q): Query<SocketsQuery>) -> Response {
    ui::page(SocketsLive {
        live: Live::new(&state, q.conn),
    })
}

/// `GET /dev/sockets.json`: the same snapshot, for agents.
async fn json(State(state): State<AppState>) -> Response {
    Json(state.hub.snapshot()).into_response()
}

/// The `/dev` tile's figure.
pub fn tile_figure(state: &AppState) -> String {
    let n = state.hub.total_conns();
    if n == 1 {
        "1 connection".to_owned()
    } else {
        format!("{n} connections")
    }
}
