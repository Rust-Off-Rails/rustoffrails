//! The development toolbox (`/dev`): index, routes, system, introspection and hot reload. Only
//! mounted when `APP_ENV=development`; every response is `no-store`. Spec: .offrails/guides/dev-tools.md

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde::Deserialize;

use crate::config::Config;
use crate::controllers::migrations;
use crate::error::AppError;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui::{self, BOOTED_AT, Chrome};
use crate::routes::ROUTES;
use crate::state::AppState;

/// Matches the `FRONTEND_MODE` in the app's `AGENTS.md` header.
pub const FRONTEND_MODE: &str = "htmx";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dev", get(index))
        .route("/dev/routes", get(routes_page))
        .route("/dev/system", get(system))
        .route("/dev/introspect.json", get(introspect))
        .route("/dev/reload", get(reload))
        .route("/dev/migrations", post(migrations::run))
        // samples: dev routes
        .layer(ui::no_store())
}

fn chrome(
    state: &AppState,
    tab: &str,
    title: &str,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Chrome {
    Chrome::new(
        title,
        tab,
        state.assets.assets(),
        true,
        flash.and_then(|f| f.0.0),
        theme,
    )
}

/// A tile on the `/dev` index.
pub struct Tile {
    pub title: &'static str,
    pub href: &'static str,
    pub description: &'static str,
    pub figure: String,
}

/// A "Try it" button: a form posting to `action`.
pub struct TryIt {
    pub label: &'static str,
    pub action: &'static str,
}

#[derive(Template)]
#[template(path = "kernel/dev/index.html")]
struct DevIndex {
    chrome: Chrome,
    tiles: Vec<Tile>,
    try_it: Vec<TryIt>,
    base: String,
}

async fn index(
    State(state): State<AppState>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let pending = migrations::pending(&state.db).await?.len();
    let tiles = vec![
        // samples: dev tiles
        Tile {
            title: "Routes",
            href: "/dev/routes",
            description: "Method, path, handler and access level of every route.",
            figure: format!("{} routes", ROUTES.len()),
        },
        Tile {
            title: "System",
            href: "/dev/system",
            description: "Environment, database and migrations.",
            figure: if pending == 0 {
                "migrations up to date".to_owned()
            } else {
                format!("{pending} pending migrations")
            },
        },
    ];
    let try_it: Vec<TryIt> = vec![
        // samples: dev try-it
    ];
    Ok(ui::page(DevIndex {
        chrome: chrome(&state, "dev", "Dev tools", flash, theme),
        tiles,
        try_it,
        base: state.config.app_url.clone(),
    }))
}

pub struct RouteRow {
    pub method: &'static str,
    pub path: &'static str,
    pub handler: &'static str,
    pub access: String,
}

#[derive(Template)]
#[template(path = "kernel/dev/routes.html")]
struct DevRoutes {
    chrome: Chrome,
    rows: Vec<RouteRow>,
    q: String,
    shown: usize,
    total: usize,
}

#[derive(Debug, Default, Deserialize)]
struct RoutesQuery {
    q: Option<String>,
}

async fn routes_page(
    State(state): State<AppState>,
    Query(query): Query<RoutesQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Response {
    let q = query.q.unwrap_or_default().trim().to_lowercase();
    let rows: Vec<RouteRow> = ROUTES
        .iter()
        .filter(|(method, path, handler, access)| {
            q.is_empty()
                || [*method, *path, *handler, access.as_str()]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&q))
        })
        .map(|(method, path, handler, access)| RouteRow {
            method,
            path,
            handler,
            access: access.as_str().to_owned(),
        })
        .collect();
    ui::page(DevRoutes {
        chrome: chrome(&state, "routes", "Routes", flash, theme),
        shown: rows.len(),
        total: ROUTES.len(),
        rows,
        q,
    })
}

#[derive(Template)]
#[template(path = "kernel/dev/system.html")]
struct DevSystem {
    chrome: Chrome,
    facts: Vec<(&'static str, String)>,
    applied: Vec<String>,
    pending: Vec<String>,
}

async fn system(
    State(state): State<AppState>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    Ok(ui::page(DevSystem {
        chrome: chrome(&state, "system", "System", flash, theme),
        facts: describe_config(&state.config),
        applied: migrations::applied(&state.db).await?,
        pending: migrations::pending(&state.db).await?,
    }))
}

/// Never shows credentials: the backend plus host (or file path) only.
pub fn redact_db(url: &str) -> String {
    if let Some(rest) = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
    {
        let path = rest.split('?').next().unwrap_or_default();
        return if path.contains(":memory:") || path.is_empty() {
            "sqlite (in memory)".to_owned()
        } else {
            format!("sqlite file {path}")
        };
    }
    if let Some((_, rest)) = url.split_once("://") {
        let host_and_db = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
        let host_and_db = host_and_db.split('?').next().unwrap_or(host_and_db);
        return format!("postgres {host_and_db}");
    }
    "unknown".to_owned()
}

fn describe_config(c: &Config) -> Vec<(&'static str, String)> {
    let uptime_ms = (Utc::now() - *BOOTED_AT).num_milliseconds();
    #[allow(unused_mut)] // samples add their facts below
    let mut facts = vec![
        ("APP_ENV", format!("{:?}", c.env).to_lowercase()),
        ("Database", redact_db(&c.database_url)),
        ("Frontend mode", FRONTEND_MODE.to_owned()),
        ("APP_URL", c.app_url.clone()),
        ("Version", env!("CARGO_PKG_VERSION").to_owned()),
        (
            "Revision",
            c.revision.clone().unwrap_or_else(|| "not set".to_owned()),
        ),
        ("Uptime", ui::duration(uptime_ms)),
    ];
    // samples: system facts
    facts
}

/// `GET /dev/introspect.json`: the app's state for agents.
async fn introspect(State(s): State<AppState>) -> Result<Response, AppError> {
    Ok(Json(crate::kernel::introspect::snapshot(&s, FRONTEND_MODE, true).await?).into_response())
}

/// `GET /dev/reload`: polled once a second by `static/dev-reload.js`.
async fn reload() -> Response {
    Json(serde_json::json!({ "version": crate::kernel::introspect::reload_version() }))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::redact_db;

    #[test]
    fn database_urls_never_show_credentials() {
        assert_eq!(
            redact_db("sqlite://storage/development.sqlite3?mode=rwc"),
            "sqlite file storage/development.sqlite3"
        );
        assert_eq!(redact_db("sqlite::memory:"), "sqlite (in memory)");
        let shown = redact_db("postgres://user:s3cret@db.example.com:5432/app?sslmode=require");
        assert_eq!(shown, "postgres db.example.com:5432/app");
        assert!(!shown.contains("s3cret") && !shown.contains("user"));
    }
}
