//! "Forgot your password?": a single-use, hashed, 30-minute link by email, and the live strength
//! meter.
//!
//! GUIDE: .offrails/guides/auth.md#password-reset-passwordsrs-new-create-edit-update

use std::time::Duration as StdDuration;

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::{
        account::{invalid_link, token_page},
        application::ClientIp,
        auth::{self, RequestMeta},
    },
    error::AppError,
    kernel::db,
    mailers::auth_mailer::{AuthMail, Notice, url},
    models::{
        Errors,
        auth_event::{self, Kind},
        auth_throttle, password_credential, password_policy, user, user_token,
    },
    routes::paths,
    state::AppState,
    views::{
        self, Ctx,
        auth::{CheckEmail, CtxExt},
        passwords::{PasswordsEdit, PasswordsNew, StrengthMeter},
    },
};

// APP: reset-request limits, per network and per address (each can send an email).
const IP_LIMIT: u32 = 10;
const IP_WINDOW: StdDuration = StdDuration::from_secs(3 * 60);
const EMAIL_LIMIT: u32 = 3;
const EMAIL_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);

#[derive(Debug, Default, Deserialize)]
pub struct ResetRequestParams {
    #[serde(default)]
    pub email: String,
}

/// `GET /passwords/new`
pub async fn new(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(PasswordsNew {
        ctx: ctx.titled("Reset your password"),
        email: String::new(),
        errors: Errors::default(),
        alert: None,
    })?
    .into_response())
}

/// `POST /passwords`: emails a reset link if the address has an account. The answer is the
/// same either way.
// SECURITY: same redirect for known and unknown addresses; the token is minted in the
// background task, so neither the response nor its timing differs.
// INVARIANT: reset requests don't enumerate accounts (test: reset_requests_look_identical_for_unknown_emails)
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    Form(params): Form<ResetRequestParams>,
) -> Result<Response, AppError> {
    let ctx = ctx.titled("Reset your password");
    let email = match user::validate_email(&params.email) {
        Ok(e) => e,
        Err(errors) => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                views::render(PasswordsNew {
                    ctx,
                    email: params.email,
                    errors,
                    alert: None,
                })?,
            )
                .into_response());
        }
    };
    if s.rate_limit
        .check(&ip.key("passwords#create"), IP_LIMIT, IP_WINDOW)
        .await
        .is_err()
        || s.rate_limit
            .check(
                &format!("passwords#create:{email}"),
                EMAIL_LIMIT,
                EMAIL_WINDOW,
            )
            .await
            .is_err()
    {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            views::render(PasswordsNew {
                ctx,
                email: params.email,
                errors: Errors::default(),
                alert: Some("Too many reset requests. Try again later.".to_owned()),
            })?,
        )
            .into_response());
    }
    let found = user::find_by_email(&s.db, &email).await?;
    // WHY: the event is written for unknown addresses too (without a user), so both paths do the
    // same database work and take the same time.
    auth_event::record(
        &s.db,
        found.as_ref().map(|u| u.id),
        Kind::PasswordResetRequested,
        &meta.meta,
        Utc::now(),
    )
    .await?;
    if let Some(u) = found {
        send_reset(&s, &u);
    }
    Ok(Redirect::to(&paths::password_sent()).into_response())
}

/// Mints the reset token in the background and emails the link.
pub fn send_reset(s: &AppState, user: &user::Model) {
    let state = s.clone();
    let (user_id, to) = (user.id, user.email.clone());
    s.background.spawn("password reset email", async move {
        let token = user_token::issue(
            &state.db,
            user_id,
            user_token::Purpose::PasswordReset,
            &to,
            None,
            Utc::now(),
        )
        .await?;
        let app = crate::config::APP_NAME;
        let email = AuthMail {
            to: to.clone(),
            subject: format!("Reset your {app} password"),
            heading: "Reset your password".to_owned(),
            intro: vec!["Someone asked to reset the password for your account. If it was you, choose a new one below.".to_owned()],
            action: Some(("Choose a new password".to_owned(), url(&state.config, &paths::edit_password(&token)))),
            outro: vec![
                "The link works once, for 30 minutes. Resetting signs you out on every other device.".to_owned(),
                "If you didn't ask for this, ignore this email: your password stays the same.".to_owned(),
            ],
            ..Default::default()
        }
        .render()?;
        state.mailer.deliver(email).await?;
        Ok(())
    });
}

/// `GET /passwords/sent`
pub async fn sent(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(CheckEmail {
        ctx: ctx.titled("Check your email").noindex(),
        heading: "Check your email".to_owned(),
        // APP: wording. It must read the same whether or not the address has an account.
        message: "If that address has an account, we've sent it a link to reset the password. The link works once, for 30 minutes.".to_owned(),
        code_form: None,
    })?
    .into_response())
}

/// `GET /passwords/{token}/edit`
pub async fn edit(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_password()).noindex();
    match user_token::find_valid(
        &s.db,
        user_token::Purpose::PasswordReset,
        &token,
        Utc::now(),
    )
    .await?
    {
        Some((_, u)) => Ok(token_page(
            views::render(PasswordsEdit {
                ctx: ctx.titled("Choose a new password"),
                email: u.email,
                action: paths::update_password(&token),
                errors: Errors::default(),
                strength: None,
            })?
            .into_response(),
        )),
        None => invalid(ctx),
    }
}

#[derive(Default, Deserialize)]
pub struct NewPasswordParams {
    #[serde(default)]
    pub password: String,
}

/// `POST /passwords/{token}`: sets the new password, ends every session, unlocks sign-in,
/// verifies the address (the link proved it), then signs in (two-factor still applies).
// SECURITY: single use (the consume is the claim), every other reset link and session dies,
// and the owner is told by email.
// INVARIANT: a reset link works once (test: a_reset_link_works_once)
// INVARIANT: a reset ends every session (test: a_reset_ends_every_other_session)
pub async fn update(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(params): Form<NewPasswordParams>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_password()).noindex();
    let now = Utc::now();
    let Some((_, u)) =
        user_token::find_valid(&s.db, user_token::Purpose::PasswordReset, &token, now).await?
    else {
        return invalid(ctx);
    };
    // Validate first: a weak password re-renders the form and the link keeps working.
    let errors = password_policy::validate(&params.password, &u.email);
    if !errors.is_empty() {
        return Ok(token_page(
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                views::render(PasswordsEdit {
                    ctx: ctx.titled("Choose a new password"),
                    email: u.email,
                    action: paths::update_password(&token),
                    errors,
                    strength: Some(password_policy::strength(&params.password, "")),
                })?,
            )
                .into_response(),
        ));
    }
    let digest = password_credential::hash(params.password).await?;
    let txn = db::begin(&s.db).await?;
    let Some((_, u)) =
        user_token::consume(&txn, user_token::Purpose::PasswordReset, &token, now).await?
    else {
        txn.rollback().await?;
        return invalid(ctx);
    };
    password_credential::set(&txn, u.id, digest, now).await?;
    user_token::revoke_all(&txn, u.id, user_token::Purpose::PasswordReset).await?;
    auth::revoke_everything(&txn, u.id).await?;
    user::mark_verified(&txn, u.id, now).await?;
    auth_throttle::clear(&txn, &auth::throttle_key(&s, "login", &u.email)).await?;
    auth_event::record(&txn, Some(u.id), Kind::PasswordReset, &meta.meta, now).await?;
    txn.commit().await?;
    auth::sessions_ended(&s, u.id);
    auth::notify(&s, u.id, Notice::PasswordChanged);
    let u = user::find_self(&s.db, u.id).await?;
    auth::sign_in(&s, &headers, &meta, u, "reset").await
}

fn invalid(ctx: Ctx) -> Result<Response, AppError> {
    invalid_link(
        ctx,
        "This reset link doesn't work",
        "Password reset links work once and expire after 30 minutes, and changing your password cancels older ones. Ask for a new link.",
        "Get a new link",
        &paths::new_password(),
    )
}

#[derive(Default, Deserialize)]
pub struct StrengthParams {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub email: String,
}

// APP: requests per minute per network for the live meter.
const STRENGTH_LIMIT: u32 = 120;
const STRENGTH_WINDOW: StdDuration = StdDuration::from_secs(60);

/// `POST /password-strength` (htmx): the meter for what's typed so far. POST, so the password
/// never lands in a URL or an access log; nothing is stored or logged.
// SECURITY: POST + CSRF like every form; rate limited; the body is never logged
// (test: the_strength_meter_works_by_post_only, passwords_and_reset_tokens_never_reach_the_logs)
pub async fn strength(
    State(s): State<AppState>,
    ip: ClientIp,
    Form(params): Form<StrengthParams>,
) -> Result<Response, AppError> {
    s.rate_limit
        .check(
            &ip.key("password#strength"),
            STRENGTH_LIMIT,
            STRENGTH_WINDOW,
        )
        .await?;
    let strength = (!params.password.is_empty())
        .then(|| password_policy::strength(&params.password, &params.email));
    Ok(views::render(StrengthMeter { strength })?.into_response())
}
