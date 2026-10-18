//! `GET /read/notes/{slug}`: one note, for anyone (the public-show-by-slug snippet).
//! APP: the spec's AC for what a visitor sees (product/features/notes.md).

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
};

use crate::{
    error::AppError,
    models::note_slugs,
    state::AppState,
    views::{self, Ctx, note_pages::NotePage, notes::NoteView},
};

/// `GET /read/notes/{slug}`
#[tracing::instrument(skip_all)]
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(slug): Path<String>,
) -> Result<Response, AppError> {
    let record = note_slugs::find_public(&s.db, &slug).await?; // unscoped: a public page by slug (APP: cite the AC)
    Ok(views::render(NotePage {
        ctx: ctx.titled(&record.label()).described(&record.body),
        note: NoteView::from_model(&record),
    })?
    .into_response())
}
