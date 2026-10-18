//! Two-factor authentication with an authenticator app (TOTP, RFC 6238).
//!
//! GUIDE: .offrails/guides/auth.md
//! GUIDE: .offrails/guides/security.md#non-negotiables
//!
//! | Step | Route |
//! |------|-------|
//! | First factor done (any method) | `auth::sign_in` calls `intercept`: a pending sign-in + 303 `/two-factor` |
//! | Type the code | `GET /two-factor`, `POST /two-factor` |
//! | Lost phone | `POST /two-factor/recovery` (a recovery code) |
//! | Turn on | `GET /account/two-factor/new` (QR + manual key), `POST /account/two-factor` (confirm a code) |
//! | New recovery codes | `POST /account/two-factor/recovery-codes` |
//! | Turn off | `POST /account/two-factor/delete` |
//! | Forget a remembered device | `POST /account/trusted-devices/{id}/delete` |
//! | Re-auth | `POST /account/confirm/two-factor` |
//!
//! Everything that changes two-factor needs a recent re-auth (`RequireRecentAuth`), and with
//! two-factor on, re-auth itself needs a code.

use std::time::Duration as StdDuration;

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::{
        account,
        application::ClientIp,
        auth::{self, CurrentSession, RequestMeta, RequireRecentAuth},
    },
    error::AppError,
    kernel::{db, flash::Flash, session as auth_cookie},
    mailers::auth_mailer::Notice,
    models::{
        Errors,
        auth_event::{self, Kind},
        auth_throttle, pending_login, recovery_code, session, totp_credential, trusted_device,
        user,
    },
    routes::paths,
    state::AppState,
    views::{
        self, Ctx,
        two_factor::{RecoveryCodes, TwoFactorChallenge, TwoFactorNew, qr_data_uri},
    },
};

const PENDING_COOKIE: &str = "pending_login";
const TRUSTED_COOKIE: &str = "trusted_device";

// APP: per-network cap on code guesses (the per-sign-in and per-account limits are separate).
const IP_LIMIT: u32 = 30;
const IP_WINDOW: StdDuration = StdDuration::from_secs(5 * 60);

// ---------------------------------------------------------------------------------------------
// The second step of signing in
// ---------------------------------------------------------------------------------------------

/// Called by `auth::sign_in` once the first factor checked out. Two-factor on and this browser
/// not trusted: start a pending sign-in and ask for the code. Otherwise `None` (sign in now).
// SECURITY: no session exists until the code is right, so a stolen password (or link) alone
// signs nobody in (test: a_password_alone_doesnt_sign_in_when_two_factor_is_on)
pub async fn intercept(
    state: &AppState,
    headers: &HeaderMap,
    _meta: &RequestMeta,
    user: &user::Model,
    method: &'static str,
) -> Result<Option<Response>, AppError> {
    if !totp_credential::enabled_for(&state.db, user.id).await? {
        return Ok(None);
    }
    let secure = state.config.is_production();
    let now = Utc::now();
    if let Some(raw) = auth_cookie::read(headers, &auth_cookie::name(TRUSTED_COOKIE, secure))
        && trusted_device::check(&state.db, user.id, &raw, now).await?
    {
        return Ok(None);
    }
    let raw = pending_login::start(&state.db, user.id, method, now).await?;
    let mut response = Redirect::to(&paths::two_factor()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set(
            &auth_cookie::name(PENDING_COOKIE, secure),
            &raw,
            Some(pending_login::TTL.num_seconds()),
            secure,
        ),
    );
    Ok(Some(response))
}

async fn load_pending(
    s: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(pending_login::Model, user::Model)>, AppError> {
    let secure = s.config.is_production();
    let Some(raw) = auth_cookie::read(headers, &auth_cookie::name(PENDING_COOKIE, secure)) else {
        return Ok(None);
    };
    // Nobody is signed in yet: the pending-login cookie's token is the key.
    let pending = pending_login::find(&s.db, &raw, Utc::now()).await?; // unscoped: before sign-in
    let Some(pending) = pending else {
        return Ok(None);
    };
    let u = user::find_self(&s.db, pending.user_id).await?;
    Ok(Some((pending, u)))
}

fn start_over() -> Response {
    (
        Flash::alert("Your sign-in timed out. Please sign in again."),
        Redirect::to(&paths::new_session()),
    )
        .into_response()
}

fn challenge(
    ctx: Ctx,
    status: StatusCode,
    mode: &'static str,
    alert: Option<String>,
    errors: Errors,
) -> Result<Response, AppError> {
    let page = TwoFactorChallenge {
        ctx: ctx.titled("Two-factor authentication").noindex(),
        alert,
        errors,
        mode,
    };
    Ok((status, views::render(page)?).into_response())
}

/// `GET /two-factor`
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if load_pending(&s, &headers).await?.is_none() {
        return Ok(start_over());
    }
    challenge(ctx, StatusCode::OK, "code", None, Errors::default())
}

#[derive(Default, Deserialize)]
pub struct CodeParams {
    #[serde(default)]
    pub code: String,
    /// The "trust this device for 30 days" checkbox (present when ticked).
    #[serde(default)]
    pub trust_device: Option<String>,
}

/// `POST /two-factor`: the authenticator code.
pub async fn verify(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    headers: HeaderMap,
    Form(params): Form<CodeParams>,
) -> Result<Response, AppError> {
    second_step(s, ctx, meta, ip, headers, params, "code").await
}

/// `POST /two-factor/recovery`: a recovery code instead.
pub async fn recovery(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    headers: HeaderMap,
    Form(params): Form<CodeParams>,
) -> Result<Response, AppError> {
    second_step(s, ctx, meta, ip, headers, params, "recovery").await
}

/// The shared second step. Three limits: per network, per pending sign-in (5 tries, then start
/// over), and per account (progressive delays across sign-ins).
// SECURITY: 10^6 codes, 3 valid at a time: 5 tries per password entry plus per-account delays
// make guessing hopeless (test: a_pending_sign_in_allows_five_wrong_codes_then_starts_over)
async fn second_step(
    s: AppState,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    headers: HeaderMap,
    params: CodeParams,
    mode: &'static str,
) -> Result<Response, AppError> {
    let Some((pending, u)) = load_pending(&s, &headers).await? else {
        return Ok(start_over());
    };
    if s.rate_limit
        .check(&ip.key("two_factor#verify"), IP_LIMIT, IP_WINDOW)
        .await
        .is_err()
    {
        return challenge(
            ctx,
            StatusCode::TOO_MANY_REQUESTS,
            mode,
            Some("Too many attempts from your network. Try again in a few minutes.".to_owned()),
            Errors::default(),
        );
    }
    let now = Utc::now();
    let key = auth::throttle_key(&s, "totp", &u.id.to_string());
    let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
        Ok(a) => a,
        Err(paused) => {
            return challenge(
                ctx,
                StatusCode::TOO_MANY_REQUESTS,
                mode,
                Some(format!(
                    "Too many wrong codes. Try again in {}.",
                    paused.wait_text(now)
                )),
                Errors::default(),
            );
        }
    };
    if !pending_login::count_attempt(&s.db, pending.id).await? {
        pending_login::finish(&s.db, pending.id).await?;
        return Ok(start_over());
    }

    let ok = match mode {
        "recovery" => {
            recovery_code::redeem(&s.db, &s.config.auth.secret_key, u.id, &params.code, now).await?
        }
        _ => match totp_credential::find_for(&s.db, u.id).await? {
            Some(cred) if cred.enabled_at.is_some() => {
                totp_credential::verify(&s.db, &s.config.totp, &cred, &u.email, &params.code, now)
                    .await?
            }
            _ => false,
        },
    };
    if !ok {
        auth_event::record(&s.db, Some(u.id), Kind::TwoFactorFailed, &meta.meta, now).await?;
        let left = pending_login::MAX_ATTEMPTS - (pending.attempts + 1);
        if left <= 0 {
            pending_login::finish(&s.db, pending.id).await?;
            return Ok(start_over());
        }
        let message = if mode == "recovery" {
            format!("isn't a valid unused recovery code ({left} tries left)")
        } else {
            format!("isn't right; check your app's current code ({left} tries left)")
        };
        return challenge(
            ctx,
            StatusCode::UNPROCESSABLE_ENTITY,
            mode,
            None,
            Errors::single("code", &message),
        );
    }

    // Right code: claim the pending sign-in (a racing duplicate loses).
    if !pending_login::finish(&s.db, pending.id).await? {
        return Ok(start_over());
    }
    auth_throttle::succeeded(&s.db, attempt).await?;
    let secure = s.config.is_production();
    let mut extra = Vec::new();
    if mode == "recovery" {
        auth_event::record(&s.db, Some(u.id), Kind::RecoveryCodeUsed, &meta.meta, now).await?;
        let remaining = recovery_code::remaining(&s.db, u.id).await?;
        auth::notify(&s, u.id, Notice::RecoveryCodeUsed { remaining });
    }
    if params.trust_device.is_some() {
        let raw = trusted_device::trust(
            &s.db,
            u.id,
            meta.meta.ip.clone(),
            meta.meta.user_agent.clone(),
            now,
        )
        .await?;
        auth_event::record(&s.db, Some(u.id), Kind::DeviceTrusted, &meta.meta, now).await?;
        extra.push(auth_cookie::set(
            &auth_cookie::name(TRUSTED_COOKIE, secure),
            &raw,
            Some(trusted_device::TTL.num_seconds()),
            secure,
        ));
    }
    let method: &'static str = match (mode, pending.method.as_str()) {
        ("recovery", _) => "recovery_code",
        (_, "password") => "password+totp",
        (_, "magic_link") => "magic_link+totp",
        (_, "magic_link_code") => "magic_link_code+totp",
        (_, "reset") => "reset+totp",
        _ => "password+totp",
    };
    let mut response = auth::finish_sign_in(&s, &headers, &meta, u, method).await?;
    let h = response.headers_mut();
    h.append(
        header::SET_COOKIE,
        auth_cookie::clear(&auth_cookie::name(PENDING_COOKIE, secure), secure),
    );
    for cookie in extra {
        h.append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

// ---------------------------------------------------------------------------------------------
// Turning it on
// ---------------------------------------------------------------------------------------------

/// `GET /account/two-factor/new`: the QR code and manual key for a pending secret.
pub async fn new(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
) -> Result<Response, AppError> {
    if totp_credential::enabled_for(&s.db, auth.user.id).await? {
        return Ok(Redirect::to(&paths::account()).into_response());
    }
    let cred =
        totp_credential::pending_for(&s.db, &s.config.totp, auth.user.id, Utc::now()).await?;
    enrol_page(
        &s,
        ctx,
        &auth.user,
        &cred,
        StatusCode::OK,
        Errors::default(),
    )
}

fn enrol_page(
    s: &AppState,
    ctx: Ctx,
    u: &user::Model,
    cred: &totp_credential::Model,
    status: StatusCode,
    errors: Errors,
) -> Result<Response, AppError> {
    let secret = totp_credential::secret_of(&s.config.totp, cred)?;
    let totp = totp_credential::totp(&secret, &u.email)?;
    let url = totp
        .to_url()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("otpauth url: {e}")))?;
    let page = TwoFactorNew {
        ctx: ctx.titled("Set up two-factor authentication").noindex(),
        qr_data_uri: qr_data_uri(&url)?,
        manual_key: totp_credential::manual_key(&secret),
        account: u.email.clone(),
        issuer: crate::config::APP_NAME.to_owned(),
        errors,
        demo_secret: s.config.is_development() && secret == totp_credential::DEMO_SECRET,
    };
    // SECURITY: the page shows a secret: never cached (test: the_enrolment_page_is_not_cached)
    let mut response = (status, views::render(page)?).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Default, Deserialize)]
pub struct ConfirmParams {
    #[serde(default)]
    pub code: String,
}

/// `POST /account/two-factor`: a code from the app confirms the setup; two-factor turns on and
/// the recovery codes are shown once.
// SECURITY: confirm before enable: a mis-scanned secret can't lock the person out
// (test: enrolment_needs_a_valid_code_before_two_factor_turns_on)
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
    meta: RequestMeta,
    Form(params): Form<ConfirmParams>,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let u = auth.user;
    let Some(cred) = totp_credential::find_for(&s.db, u.id)
        .await?
        .filter(|c| c.enabled_at.is_none())
    else {
        return Ok(Redirect::to(&paths::account()).into_response());
    };
    let key = auth::throttle_key(&s, "totp", &u.id.to_string());
    let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
        Ok(a) => a,
        Err(paused) => {
            let errors = Errors::single(
                "code",
                &format!(
                    "was wrong too many times; try again in {}",
                    paused.wait_text(now)
                ),
            );
            return enrol_page(&s, ctx, &u, &cred, StatusCode::TOO_MANY_REQUESTS, errors);
        }
    };
    if !totp_credential::verify(&s.db, &s.config.totp, &cred, &u.email, &params.code, now).await? {
        let errors = Errors::single(
            "code",
            "isn't right. Check that your phone's clock is set automatically, then try the newest code",
        );
        return enrol_page(&s, ctx, &u, &cred, StatusCode::UNPROCESSABLE_ENTITY, errors);
    }
    auth_throttle::succeeded(&s.db, attempt).await?;
    let txn = db::begin(&s.db).await?;
    totp_credential::enable(&txn, u.id, now).await?;
    let codes = recovery_code::regenerate(&txn, &s.config.auth.secret_key, u.id, now).await?;
    // A privilege change: this session gets a fresh token, other devices are signed out.
    let raw = session::rotate(&txn, auth.session.id, now).await?;
    session::revoke_others(&txn, &u, auth.session.id).await?;
    auth_event::record(&txn, Some(u.id), Kind::TwoFactorEnabled, &meta.meta, now).await?;
    txn.commit().await?;
    auth::sessions_ended(&s, u.id);
    auth::notify(&s, u.id, Notice::TwoFactorEnabled);
    let page = RecoveryCodes::new(
        ctx.titled("Save your recovery codes").noindex(),
        codes,
        &u.email,
        true,
    );
    let mut response = views::render(page)?.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set_session(&raw, s.config.is_production()),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `POST /account/two-factor/recovery-codes`: a new set (the old one stops working).
pub async fn regenerate(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
    meta: RequestMeta,
) -> Result<Response, AppError> {
    let now = Utc::now();
    if !totp_credential::enabled_for(&s.db, auth.user.id).await? {
        return Ok(Redirect::to(&paths::account()).into_response());
    }
    let txn = db::begin(&s.db).await?;
    let codes =
        recovery_code::regenerate(&txn, &s.config.auth.secret_key, auth.user.id, now).await?;
    auth_event::record(
        &txn,
        Some(auth.user.id),
        Kind::RecoveryCodesRegenerated,
        &meta.meta,
        now,
    )
    .await?;
    txn.commit().await?;
    auth::notify(&s, auth.user.id, Notice::RecoveryCodesRegenerated);
    let page = RecoveryCodes::new(
        ctx.titled("Your new recovery codes").noindex(),
        codes,
        &auth.user.email,
        false,
    );
    let mut response = views::render(page)?.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `POST /account/two-factor/delete`: turns two-factor off (after a re-auth with a code).
// SECURITY: needs a recent re-auth, which with two-factor on needs a code, so a stolen session
// or password alone can't remove the second factor (test: turning_two_factor_off_needs_a_fresh_code)
pub async fn destroy(
    State(s): State<AppState>,
    auth: RequireRecentAuth,
    meta: RequestMeta,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let u = auth.user;
    let txn = db::begin(&s.db).await?;
    totp_credential::remove(&txn, u.id).await?;
    recovery_code::remove_all(&txn, u.id).await?;
    trusted_device::revoke_all(&txn, u.id).await?;
    pending_login::revoke_all(&txn, u.id).await?;
    auth_event::record(&txn, Some(u.id), Kind::TwoFactorDisabled, &meta.meta, now).await?;
    txn.commit().await?;
    auth::notify(&s, u.id, Notice::TwoFactorDisabled);
    Ok((
        Flash::notice("Two-factor authentication is off."),
        Redirect::to(&paths::account()),
    )
        .into_response())
}

/// `POST /account/trusted-devices/{id}/delete`
pub async fn forget_device(
    State(s): State<AppState>,
    current: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    // SECURITY: scoped to the signed-in person; another person's id is a 404
    // (test: forgetting_another_users_trusted_device_is_404)
    trusted_device::revoke_for(&s.db, &current.user, id).await?;
    Ok((
        Flash::notice("That device will need a code next time."),
        Redirect::to(&paths::account()),
    )
        .into_response())
}

// ---------------------------------------------------------------------------------------------
// Re-authentication with a code
// ---------------------------------------------------------------------------------------------

/// `POST /account/confirm/two-factor`: confirm identity with an authenticator (or recovery) code.
pub async fn confirm(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
    meta: RequestMeta,
    headers: HeaderMap,
    Form(params): Form<ConfirmParams>,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let u = &current.user;
    let key = auth::throttle_key(&s, "totp", &u.id.to_string());
    let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
        Ok(a) => a,
        Err(paused) => {
            return account::confirm_error(
                &s,
                ctx,
                &current,
                "two_factor",
                StatusCode::TOO_MANY_REQUESTS,
                Some(format!(
                    "Too many wrong codes. Try again in {}.",
                    paused.wait_text(now)
                )),
                Errors::default(),
            )
            .await;
        }
    };
    let ok = match totp_credential::find_for(&s.db, u.id).await? {
        Some(cred) if cred.enabled_at.is_some() => {
            totp_credential::verify(&s.db, &s.config.totp, &cred, &u.email, &params.code, now)
                .await?
                || recovery_code::redeem(&s.db, &s.config.auth.secret_key, u.id, &params.code, now)
                    .await?
        }
        _ => false,
    };
    if !ok {
        auth_event::record(&s.db, Some(u.id), Kind::TwoFactorFailed, &meta.meta, now).await?;
        return account::confirm_error(
            &s,
            ctx,
            &current,
            "two_factor",
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
            Errors::single("code", "isn't right"),
        )
        .await;
    }
    auth_throttle::succeeded(&s.db, attempt).await?;
    auth::grant_step_up(&s, &headers, &meta, &current).await
}
