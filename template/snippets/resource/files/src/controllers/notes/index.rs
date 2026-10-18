//! `GET /notes`: a page of notes, newest first.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};

use crate::{
    error::AppError,
    models::{Page, note},
    state::AppState,
    views::{
        self, Ctx,
        notes::{NoteView, NotesIndex},
    },
};

/// `GET /notes`
#[tracing::instrument(skip_all)]
pub async fn index(State(s): State<AppState>, ctx: Ctx, page: Page) -> Result<Response, AppError> {
    let (rows, more) = note::all(&s.db, page).await?; // unscoped: public by --public (APP: cite the AC)
    Ok(views::render(NotesIndex {
        ctx: ctx.titled("Notes"),
        notes: rows.iter().map(NoteView::from_model).collect(),
        page: page.number(),
        more,
    })?
    .into_response())
}
