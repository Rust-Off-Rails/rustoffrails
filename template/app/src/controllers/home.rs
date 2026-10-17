use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::views::{self, Ctx, home::HomeIndex};

// Spans name the action for the dev dashboard's traces; skip_all keeps arguments (and any
// personal data in them) out of span fields (errors-and-logging.md#spans).
#[tracing::instrument(skip_all)]
pub async fn index(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(HomeIndex { ctx })?.into_response())
}
