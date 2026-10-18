//! `GET /notes/{id}/edit` and `POST /notes/{id}`: the form, and saving changes to a note.

use axum::{
    Form,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};

use crate::{
    error::AppError,
    kernel::flash::Flash,
    models::{Errors, note, note::NoteParams},
    routes::paths,
    state::AppState,
    views::{self, Ctx, notes::NotesEdit},
};

/// `GET /notes/{id}/edit`
#[tracing::instrument(skip_all)]
pub async fn edit(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let record = note::find(&s.db, id).await?; // unscoped: public by --public (APP: cite the AC)
    Ok(views::render(NotesEdit {
        ctx: ctx.titled(&format!("Edit {}", record.label())),
        id: record.id,
        params: NoteParams::from(&record),
        errors: Errors::default(),
        action: paths::note(record.id),
    })?
    .into_response())
}

/// `POST /notes/{id}`: 303 to the note, or the form again with 422 and the errors.
#[tracing::instrument(skip_all)]
pub async fn update(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(params): Form<NoteParams>,
) -> Result<Response, AppError> {
    let record = note::find(&s.db, id).await?; // unscoped: public by --public (APP: cite the AC)
    let result = match params.validate() {
        Ok(new) => note::update(&s.db, record, new).await,
        Err(errors) => Err(AppError::Invalid(errors)),
    };
    match result {
        Ok(record) => Ok((
            Flash::notice("Note was successfully updated."),
            Redirect::to(&paths::note(record.id)),
        )
            .into_response()),
        Err(AppError::Invalid(errors)) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            views::render(NotesEdit {
                ctx: ctx.titled("Edit note"),
                id,
                params,
                errors,
                action: paths::note(id),
            })?,
        )
            .into_response()),
        Err(e) => Err(e),
    }
}
