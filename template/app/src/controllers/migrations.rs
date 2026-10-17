//! Pending migrations: like Rails' pending-migration error page and Phoenix's "There are pending
//! migrations" screen. A database behind the code shows a clear page with a Run button, not a
//! "no such table" 500. See `.offrails/guides/dev-tools.md#pending-migrations`.
//!
//! | Environment | Behavior |
//! |-------------|----------|
//! | development | `guard` answers every request (except static files, `/up` and the Run button) with a 503 page |
//! | production | never auto-migrates; `GET /up` is 503 while any migration is pending |
//! | worker | waits (logging once) until the database is migrated |

use std::{future::Future, time::Duration};

use askama::Template;
use axum::{
    Form,
    extract::{OriginalUri, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use migration::{Migrator, MigratorTrait};
use sea_orm::DatabaseConnection;
use serde::Deserialize;

use crate::{
    error::AppError,
    kernel::{
        flash::{Flash, RequestFlash},
        theme::ThemeSwitch,
        ui::{self, Chrome},
    },
    state::AppState,
};

#[derive(Template)]
#[template(path = "kernel/dev/pending.html")]
struct Pending {
    chrome: Chrome,
    names: Vec<String>,
    return_to: String,
}

/// Names of the migrations not yet applied. Read-only: it never creates `seaql_migrations` (a
/// fresh database reports every migration pending).
pub async fn pending(db: &DatabaseConnection) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(Migrator::get_pending_migrations_read_only(db)
        .await?
        .iter()
        .map(|m| m.name().to_owned())
        .collect())
}

pub async fn applied(db: &DatabaseConnection) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(Migrator::get_applied_migrations_read_only(db)
        .await?
        .iter()
        .map(|m| m.name().to_owned())
        .collect())
}

/// Development only. If anything is pending, every request except static files, `/up` and the
/// Run button gets a 503 page that lists the migrations. The page is rendered without touching
/// any app table, so it works before the schema exists.
pub async fn guard(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    req: Request,
    next: Next,
) -> Response {
    let path = uri.path();
    // Static files, the health check, the Run button, and the two endpoints agents and open tabs
    // poll (they read no app table) answer while migrations are pending.
    let exempt = path.starts_with("/static/")
        || path == "/up"
        || matches!(path, "/dev/introspect.json" | "/dev/reload")
        || (req.method() == Method::POST && matches!(path, "/dev/migrations" | "/theme"));
    if exempt {
        return next.run(req).await;
    }
    let names = match pending(&state.db).await {
        Ok(names) if !names.is_empty() => names,
        // Nothing pending, or the check itself failed: let the request through and let the
        // page show its own error.
        _ => return next.run(req).await,
    };
    let flash = req
        .extensions()
        .get::<RequestFlash>()
        .cloned()
        .and_then(|f| f.0);
    let return_to = uri
        .path_and_query()
        .map_or("/", |pq| pq.as_str())
        .to_owned();
    let chrome = Chrome::new(
        "Pending migrations",
        "system",
        state.assets.assets(),
        true,
        flash,
        ThemeSwitch::new(req.headers(), &uri),
    );
    let mut response = ui::page(Pending {
        chrome,
        names,
        return_to,
    });
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response
}

#[derive(Debug, Deserialize)]
pub struct RunForm {
    #[serde(default)]
    return_to: String,
}

/// `POST /dev/migrations`: the Run button. Applies everything pending and goes back.
pub async fn run(
    State(state): State<AppState>,
    Form(form): Form<RunForm>,
) -> Result<Response, AppError> {
    let before = pending(&state.db).await?.len();
    Migrator::up(&state.db, None).await?;
    tracing::info!(count = before, "migrations run from the dev page");
    let back = ui::same_site_path(&form.return_to).unwrap_or_else(|| "/dev/system".to_owned());
    Ok((
        Flash::notice(format!(
            "Ran {before} migration{}.",
            if before == 1 { "" } else { "s" }
        )),
        Redirect::to(&back),
    )
        .into_response())
}

/// Before a worker starts: if migrations are pending, log once, recheck every 5s and stop
/// cleanly on shutdown. Never spams errors against missing tables. Returns `false` if it was
/// told to stop while waiting.
pub async fn wait_until_migrated(db: &DatabaseConnection, stop: impl Future<Output = ()>) -> bool {
    tokio::pin!(stop);
    let mut warned = false;
    loop {
        match pending(db).await {
            Ok(names) if names.is_empty() => {
                if warned {
                    tracing::info!("migrations applied; worker starting");
                }
                return true;
            }
            Ok(_) | Err(_) => {
                if !warned {
                    tracing::warn!("pending migrations: run `app db migrate`; worker waiting");
                    warned = true;
                }
            }
        }
        tokio::select! {
            _ = &mut stop => return false,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}
