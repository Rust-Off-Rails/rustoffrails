//! `GET /notes/{id}`: one note.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
};

use crate::{
    error::AppError,
    models::note,
    state::AppState,
    views::{
        self, Ctx,
        notes::{NoteView, NotesShow},
    },
};

/// `GET /notes/{id}`
#[tracing::instrument(skip_all)]
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let record = note::find(&s.db, id).await?; // unscoped: public by --public (APP: cite the AC)
    Ok(views::render(NotesShow {
        ctx: ctx.titled(&record.label()),
        note: NoteView::from_model(&record),
    })?
    .into_response())
}
