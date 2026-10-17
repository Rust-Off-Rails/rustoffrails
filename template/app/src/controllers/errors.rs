use axum::response::IntoResponse;

use crate::error::{AppError, ROUTE_FALLBACK};

/// The router fallback. `shape_errors` copies the marker onto the page it renders, so tests can
/// tell "no such route" from a handler's own "no such record".
pub async fn not_found() -> impl IntoResponse {
    ([(ROUTE_FALLBACK, "1")], AppError::NotFound)
}
