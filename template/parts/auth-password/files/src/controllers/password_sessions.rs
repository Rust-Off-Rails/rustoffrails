//! Signing in with email + password, and confirming identity with the password (step-up).
//!
//! GUIDE: .offrails/guides/auth.md#login-sessionsrs-new-create-destroy
//! GUIDE: .offrails/guides/security.md#rate-limiting-kernelrate_limitrs

use std::time::Duration as StdDuration;

use axum::{
    Form,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::{
        account,
        application::ClientIp,
        auth::{self, CurrentSession, RequestMeta},
        registrations,
    },
    error::AppError,
    mailers::auth_mailer::Notice,
    models::{
        Errors,
        auth_event::{self, Kind},
        auth_throttle, password_credential, user,
    },
    state::AppState,
    views::{self, Ctx, auth::SessionsNew},
};

/// Untrusted input from the sign-in form. `Debug` is hand-written so the password can't be logged.
#[derive(Default, Deserialize)]
pub struct SignInParams {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
}

impl std::fmt::Debug for SignInParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SECURITY: a derived Debug would print the password into any log line that formats it
        // (test: sign_in_params_debug_hides_the_password)
        f.debug_struct("SignInParams")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .finish()
    }
}

// APP: per-IP limit on sign-in attempts (any account). The per-account throttle is separate.
const IP_LIMIT: u32 = 10;
const IP_WINDOW: StdDuration = StdDuration::from_secs(3 * 60);

// APP: the failure message. Identical for "no such account" and "wrong password".
const FAILED: &str = "Try another email address or password.";

/// `POST /session/password`
// SECURITY: same status, body and work (one argon2 verify, one throttle update, one audit row)
// whether or not the email has an account, so neither the page nor its timing enumerates
// accounts.
// INVARIANT: unknown and known emails get identical failures (test: unknown_and_known_emails_take_the_same_path)
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    headers: HeaderMap,
    Form(params): Form<SignInParams>, // body extractor last
) -> Result<Response, AppError> {
    let ctx = ctx.titled("Sign in");
    let email = user::normalize_email(&params.email);
    let page = |ctx: Ctx, status: StatusCode, alert: &str| -> Result<Response, AppError> {
        let page = SessionsNew {
            ctx,
            email: params.email.clone(),
            alert: Some(alert.to_owned()),
            errors: Errors::default(),
            form: "password",
        };
        Ok((status, views::render(page)?).into_response())
    };

    // SECURITY: per-IP limit first: a credential-stuffing run from one address stops here,
    // before any argon2 work (test: sign_in_is_rate_limited_per_network)
    if s.rate_limit
        .check(&ip.key("session#password"), IP_LIMIT, IP_WINDOW)
        .await
        .is_err()
    {
        let mut r = page(
            ctx,
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts from your network. Try again in a few minutes.",
        )?;
        r.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("180"));
        return Ok(r);
    }
    if email.is_empty() || params.password.is_empty() {
        return page(
            ctx,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Enter your email address and password.",
        );
    }

    // SECURITY: per-account progressive delay, keyed by the address whether or not it exists
    // (test: wrong_passwords_are_throttled_progressively_even_for_unknown_emails)
    let now = Utc::now();
    let key = auth::throttle_key(&s, "login", &email);
    let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
        Ok(attempt) => attempt,
        Err(paused) => {
            let message = format!(
                "Too many failed attempts for this account. Try again in {}, or reset your password to unlock it now.",
                paused.wait_text(now)
            );
            let mut r = page(ctx, StatusCode::TOO_MANY_REQUESTS, &message)?;
            if let Ok(v) =
                HeaderValue::from_str(&(paused.until - now).num_seconds().max(1).to_string())
            {
                r.headers_mut().insert(header::RETRY_AFTER, v);
            }
            return Ok(r);
        }
    };

    let found = user::find_by_email(&s.db, &email).await?;
    let checked =
        password_credential::check(&s.db, found.as_ref().map(|u| u.id), &params.password).await?;
    let user = match found {
        Some(u) if checked.ok => u,
        found => {
            let user_id = found.as_ref().map(|u| u.id);
            auth_event::record(&s.db, user_id, Kind::SignInFailed, &meta.meta, now).await?;
            // The attempt that starts the first pause warns the owner (once per episode).
            if attempt.number == auth_throttle::FREE_FAILURES
                && let Some(id) = user_id
            {
                auth_event::record(&s.db, Some(id), Kind::Throttled, &meta.meta, now).await?;
                auth::notify(&s, id, Notice::Throttled);
            }
            return page(ctx, StatusCode::UNPROCESSABLE_ENTITY, FAILED);
        }
    };
    auth_throttle::succeeded(&s.db, attempt).await?;
    if checked.needs_rehash {
        let digest = password_credential::hash(params.password.clone()).await?;
        password_credential::rehash(&s.db, user.id, digest).await?;
    }
    if !user.is_verified() {
        // Only someone who knows the password sees this, so it reveals nothing new.
        registrations::resend_verification(&s, &user);
        return page(
            ctx,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Confirm your email address first: we've sent you a new link.",
        );
    }
    auth::sign_in(&s, &headers, &meta, user, "password").await
}

#[derive(Default, Deserialize)]
pub struct ConfirmParams {
    #[serde(default)]
    pub password: String,
}

/// `POST /account/confirm/password`: re-authentication with the password.
// SECURITY: throttled per account; refused when two-factor is on (a second factor must confirm)
// (test: password_reauth_is_throttled, two_factor_users_must_step_up_with_a_code)
pub async fn confirm(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
    meta: RequestMeta,
    headers: HeaderMap,
    Form(params): Form<ConfirmParams>,
) -> Result<Response, AppError> {
    let error = |status: StatusCode, message: &str| (status, Some(message.to_owned()));
    if auth::step_up_needs_second_factor(&s, &current.user).await? {
        let (status, alert) = error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Two-factor authentication is on: confirm with a code from your authenticator app.",
        );
        return account::confirm_error(
            &s,
            ctx,
            &current,
            "password",
            status,
            alert,
            Errors::default(),
        )
        .await;
    }
    let now = Utc::now();
    let key = auth::throttle_key(&s, "reauth", &current.user.id.to_string());
    let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
        Ok(a) => a,
        Err(paused) => {
            let (status, alert) = error(
                StatusCode::TOO_MANY_REQUESTS,
                &format!(
                    "Too many wrong passwords. Try again in {}.",
                    paused.wait_text(now)
                ),
            );
            return account::confirm_error(
                &s,
                ctx,
                &current,
                "password",
                status,
                alert,
                Errors::default(),
            )
            .await;
        }
    };
    let checked =
        password_credential::check(&s.db, Some(current.user.id), &params.password).await?;
    if !checked.ok {
        auth_event::record(
            &s.db,
            Some(current.user.id),
            Kind::SignInFailed,
            &meta.meta,
            now,
        )
        .await?;
        return account::confirm_error(
            &s,
            ctx,
            &current,
            "password",
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
            Errors::single("password", "is incorrect"),
        )
        .await;
    }
    auth_throttle::succeeded(&s.db, attempt).await?;
    auth::grant_step_up(&s, &headers, &meta, &current).await
}
