use std::time::Duration;

use crate::{controllers::migrations, kernel::db, state::AppState};
use axum::{extract::State, http::StatusCode};

/// `GET /up`: `kernel::db::ping` (a `SELECT 1`) with a 2s timeout. No auth, CSRF or session.
pub async fn up(State(s): State<AppState>) -> (StatusCode, &'static str) {
    let check = tokio::time::timeout(Duration::from_secs(2), db::ping(&s.db)).await;
    if !matches!(check, Ok(Ok(_))) {
        return (StatusCode::SERVICE_UNAVAILABLE, "database unavailable");
    }
    // A deploy that skipped migrating must never go healthy.
    match migrations::pending(&s.db).await {
        Ok(names) if names.is_empty() => (StatusCode::OK, "ok"),
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, "pending migrations"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}
