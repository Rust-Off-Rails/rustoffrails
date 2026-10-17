//! "This wasn't me": the link in every security notice. It signs the account out everywhere,
//! forgets trusted devices, and tells the person what to do next.
//!
//! GUIDE: .offrails/guides/security.md#non-negotiables (9: session revocation)

use axum::{
    extract::{Path, State},
    http::header::SET_COOKIE,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;

use crate::{
    controllers::{
        account::{invalid_link, token_page},
        auth::{self, RequestMeta},
    },
    error::AppError,
    kernel::{db, flash::Flash, session as auth_cookie},
    models::{
        auth_event::{self, Kind},
        user_token,
    },
    routes::paths,
    state::AppState,
    views::{
        self, Ctx,
        auth::{CtxExt, NotMeShow},
    },
};

const INVALID: &str = "Security links work once and expire after 7 days. If you think someone else is using your account, sign in and sign out every other device from your account page, or reset your password.";

/// `GET /security/not-me/{token}`: a confirm button. GET changes nothing (link scanners).
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_session()).noindex();
    match user_token::find_valid(&s.db, user_token::Purpose::NotMe, &token, Utc::now()).await? {
        Some((_, u)) => Ok(token_page(
            views::render(NotMeShow {
                ctx: ctx.titled("Secure your account"),
                email: u.email,
                action: paths::not_me(&token),
            })?
            .into_response(),
        )),
        None => invalid_link(
            ctx,
            "This security link doesn't work",
            INVALID,
            "Go to sign in",
            &paths::new_session(),
        ),
    }
}

/// `POST /security/not-me/{token}`: signs out every session (this browser's too) and forgets
/// trusted devices. Works without being signed in: the person may be locked out.
// SECURITY: the token is single-use and random; it can only end sessions, never start one, so
// a leaked link is a nuisance at worst (test: not_me_signs_out_everywhere)
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let txn = db::begin(&s.db).await?;
    let Some((_, u)) = user_token::consume(&txn, user_token::Purpose::NotMe, &token, now).await?
    else {
        txn.rollback().await?;
        return invalid_link(
            ctx.at(&paths::new_session()).noindex(),
            "This security link doesn't work",
            INVALID,
            "Go to sign in",
            &paths::new_session(),
        );
    };
    auth::revoke_everything(&txn, u.id).await?;
    auth_event::record(&txn, Some(u.id), Kind::NotMe, &meta.meta, now).await?;
    txn.commit().await?;
    auth::sessions_ended(&s, u.id);
    // APP: what to suggest next. With passwords, a reset; with magic links, signing in again.
    let mut response = (
        Flash::notice(
            "Done: your account is signed out on every device. Sign in again, and if you use a password, reset it.",
        ),
        Redirect::to(&paths::new_session()),
    )
        .into_response();
    response.headers_mut().append(
        SET_COOKIE,
        auth_cookie::clear_session(s.config.is_production()),
    );
    Ok(response)
}
