//! `POST /notes/{id}/delete`: deleting a note.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
};

use crate::{error::AppError, kernel::flash::Flash, models::note, routes::paths, state::AppState};

/// `POST /notes/{id}/delete`
#[tracing::instrument(skip_all)]
pub async fn destroy(State(s): State<AppState>, Path(id): Path<i64>) -> Result<Response, AppError> {
    let record = note::find(&s.db, id).await?; // unscoped: public by --public (APP: cite the AC)
    note::destroy(&s.db, record).await?; // unscoped: public by --public (APP: cite the AC)
    Ok((
        Flash::notice("Note was successfully deleted."),
        Redirect::to(&paths::notes()),
    )
        .into_response())
}
