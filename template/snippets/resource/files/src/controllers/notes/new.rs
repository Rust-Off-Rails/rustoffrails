//! `GET /notes/new` and `POST /notes`: the form, and creating a note.

use axum::{
    Form,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};

use crate::{
    error::AppError,
    kernel::flash::Flash,
    models::{Errors, note, note::NoteParams},
    routes::paths,
    state::AppState,
    views::{self, Ctx, notes::NotesNew},
};

/// `GET /notes/new`
#[tracing::instrument(skip_all)]
pub async fn new(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(NotesNew {
        ctx: ctx.titled("New note"),
        params: NoteParams::default(),
        errors: Errors::default(),
        action: paths::notes(),
    })?
    .into_response())
}

/// `POST /notes`: 303 to the new note, or the form again with 422 and the errors.
#[tracing::instrument(skip_all)]
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    Form(params): Form<NoteParams>,
) -> Result<Response, AppError> {
    let result = match params.validate() {
        Ok(new) => note::create(&s.db, new).await,
        Err(errors) => Err(AppError::Invalid(errors)),
    };
    match result {
        Ok(record) => Ok((
            Flash::notice("Note was successfully created."),
            Redirect::to(&paths::note(record.id)),
        )
            .into_response()),
        Err(AppError::Invalid(errors)) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            views::render(NotesNew {
                ctx: ctx.titled("New note"),
                params,
                errors,
                action: paths::notes(),
            })?,
        )
            .into_response()),
        Err(e) => Err(e),
    }
}
