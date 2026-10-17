//! Changing (or, for a passwordless account, adding) a password from the account page.
//!
//! GUIDE: .offrails/guides/auth.md#password-variants-signup-invite

use axum::{
    Form,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::auth::{self, RequestMeta, RequireRecentAuth},
    error::AppError,
    kernel::{db, flash::Flash, session as auth_cookie},
    mailers::auth_mailer::Notice,
    models::{
        Errors,
        auth_event::{self, Kind},
        auth_throttle, password_credential, password_policy, session, user_token,
    },
    routes::paths,
    state::AppState,
    views::{self, Ctx, passwords::AccountPasswordEdit},
};

/// `GET /account/password/edit`
pub async fn edit(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
) -> Result<Response, AppError> {
    let has_password = password_credential::changed_at(&s.db, auth.user.id)
        .await?
        .is_some();
    Ok(views::render(AccountPasswordEdit {
        ctx: ctx
            .titled(if has_password {
                "Change password"
            } else {
                "Set a password"
            })
            .noindex(),
        user: auth.user,
        has_password,
        errors: Errors::default(),
        strength: None,
    })?
    .into_response())
}

#[derive(Default, Deserialize)]
pub struct ChangeParams {
    #[serde(default)]
    pub current_password: String,
    #[serde(default)]
    pub password: String,
}

/// `POST /account/password`: needs the current password (when there is one) **and** a recent
/// re-auth; signs out every other device and rotates this one.
// SECURITY: the current password is required even inside the re-auth window, so a session left
// open on a shared computer can't lock the owner out.
// INVARIANT: changing the password needs the current one (test: changing_the_password_needs_the_current_one)
pub async fn update(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
    meta: RequestMeta,
    Form(params): Form<ChangeParams>,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let user = auth.user;
    let has_password = password_credential::changed_at(&s.db, user.id)
        .await?
        .is_some();
    let mut errors = Errors::default();
    if has_password {
        let key = auth::throttle_key(&s, "reauth", &user.id.to_string());
        match auth_throttle::begin(&s.db, &key, now).await? {
            Err(paused) => errors.add(
                "current_password",
                &format!(
                    "was wrong too many times; try again in {}",
                    paused.wait_text(now)
                ),
            ),
            Ok(attempt) => {
                let checked =
                    password_credential::check(&s.db, Some(user.id), &params.current_password)
                        .await?;
                if checked.ok {
                    auth_throttle::succeeded(&s.db, attempt).await?;
                } else {
                    auth_event::record(&s.db, Some(user.id), Kind::SignInFailed, &meta.meta, now)
                        .await?;
                    errors.add("current_password", "is incorrect");
                }
            }
        }
    }
    if let Some(problem) = password_policy::problem(&params.password, &user.email) {
        errors.add("password", problem);
    } else if has_password && params.password == params.current_password {
        errors.add("password", "must be different from your current password");
    }
    if !errors.is_empty() {
        let strength = (!params.password.is_empty())
            .then(|| password_policy::strength(&params.password, &user.email));
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            views::render(AccountPasswordEdit {
                ctx: ctx
                    .titled(if has_password {
                        "Change password"
                    } else {
                        "Set a password"
                    })
                    .noindex(),
                user,
                has_password,
                errors,
                strength,
            })?,
        )
            .into_response());
    }

    let digest = password_credential::hash(params.password).await?;
    let txn = db::begin(&s.db).await?;
    password_credential::set(&txn, user.id, digest, now).await?;
    user_token::revoke_all(&txn, user.id, user_token::Purpose::PasswordReset).await?;
    // Every other device is signed out; this one gets a fresh token.
    auth::revoke_other_access(&txn, &user, auth.session.id).await?;
    let raw = session::rotate(&txn, auth.session.id, now).await?;
    auth_event::record(&txn, Some(user.id), Kind::PasswordChanged, &meta.meta, now).await?;
    txn.commit().await?;
    auth::sessions_ended(&s, user.id);
    auth::notify(&s, user.id, Notice::PasswordChanged);
    let notice = if has_password {
        "Password changed. Every other device was signed out."
    } else {
        "Password set. You can now sign in with it too."
    };
    let mut response = (Flash::notice(notice), Redirect::to(&paths::account())).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set_session(&raw, s.config.is_production()),
    );
    Ok(response)
}
