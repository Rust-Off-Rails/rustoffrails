//! Sign-in page and sign-out. The forms on the sign-in page come from the installed method
//! samples (password, magic link); each posts to its own controller and ends in
//! `auth::sign_in`.
//!
//! GUIDE: .offrails/guides/auth.md#login-sessionsrs-new-create-destroy

use axum::{
    extract::State,
    http::{HeaderMap, header::SET_COOKIE},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;

use crate::{
    controllers::auth::{CurrentSession, CurrentUser, RequestMeta},
    error::AppError,
    kernel::{flash::Flash, session as auth_cookie},
    models::{
        auth_event::{self, Kind},
        session,
    },
    routes::paths,
    state::AppState,
    views::{self, Ctx, auth::SessionsNew},
};

/// `GET /session/new`
pub async fn new(ctx: Ctx, CurrentUser(user): CurrentUser) -> Result<Response, AppError> {
    if user.is_some() {
        return Ok(Redirect::to(&paths::after_sign_in()).into_response());
    }
    // APP: the page title and copy live in templates/sessions/new.html.
    let ctx = ctx.titled("Sign in");
    Ok(views::render(SessionsNew::blank(ctx))?.into_response())
}

/// `POST /session/delete`: ends this session (the row, so a copied cookie dies too) and clears
/// the cookie.
// SECURITY: deleting only the cookie would leave a stolen copy of it valid.
// INVARIANT: a signed-out cookie can't be replayed (test: sign_out_kills_the_token_server_side)
pub async fn destroy(
    State(s): State<AppState>,
    current: CurrentSession,
    meta: RequestMeta,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let secure = s.config.is_production();
    if let Some(raw) = auth_cookie::read_session(&headers, secure) {
        session::destroy_by_token(&s.db, &raw).await?;
    }
    auth_event::record(
        &s.db,
        Some(current.user.id),
        Kind::SignedOut,
        &meta.meta,
        Utc::now(),
    )
    .await?;
    crate::controllers::auth::sessions_ended(&s, current.user.id);
    let mut response = (Flash::notice("Signed out."), Redirect::to(&paths::root())).into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, auth_cookie::clear_session(secure));
    Ok(response)
}
