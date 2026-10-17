//! Who is signed in, and how a sign-in completes. Every auth sample builds on this file:
//!
//! - extractors: `CurrentUser`, `RequireUser`, `CurrentSession`, `RequireRecentAuth`, `RequestMeta`;
//! - `sign_in`: the one way a session starts, whatever proved the person's identity (password,
//!   magic link, email verification, reset). Second factors plug in at its anchor;
//! - `grant_step_up`: the one way a re-authentication completes;
//! - `notify`: security emails ("new sign-in", "password changed") with a "this wasn't me" link.
//!
//! GUIDE: .offrails/guides/auth.md#extractors-in-controllersapplicationrs
//!
//! Authentication is an extractor on each handler, never middleware, so a route's protection is
//! visible in its signature.

use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
};
use chrono::{Duration, Utc};

use crate::{
    controllers::application::ClientIp,
    error::AppError,
    kernel::{auth_token, db, flash::Flash, return_to, session as auth_cookie},
    mailers::auth_mailer::{self, Notice},
    models::{
        auth_event::{self, Kind, Meta},
        session, user, user_token,
    },
    routes::paths,
    state::AppState,
};

/// A sensitive action (change email or password, turn off two-factor, new recovery codes) needs
/// a sign-in or re-authentication this recent.
// APP: the "sudo mode" window. GitHub uses a few hours; banks use minutes.
pub const REAUTH_WINDOW: Duration = Duration::minutes(10);

/// The browser's long-lived device id (a random value; its digest goes in the audit trail).
const DEVICE_COOKIE: &str = "device";
const DEVICE_MAX_AGE_SECS: i64 = 400 * 24 * 60 * 60;

// ---------------------------------------------------------------------------------------------
// Loading the session (once per request)
// ---------------------------------------------------------------------------------------------

/// The request's session and user, `None` included, cached in the request extensions so `Ctx`,
/// `CurrentUser` and `RequireUser` in one handler cost one query.
#[derive(Clone)]
struct Loaded(Option<(session::Model, user::Model)>);

async fn load(
    parts: &mut Parts,
    state: &AppState,
) -> Result<Option<(session::Model, user::Model)>, AppError> {
    if let Some(Loaded(found)) = parts.extensions.get::<Loaded>() {
        return Ok(found.clone());
    }
    let secure = state.config.is_production();
    let now = Utc::now();
    let found = match auth_cookie::read_session(&parts.headers, secure) {
        None => None,
        Some(raw) => session::find_by_token(&state.db, &raw, now).await?,
    };
    if let Some((s, _)) = &found {
        session::touch(&state.db, s, now).await?;
    }
    parts.extensions.insert(Loaded(found.clone()));
    Ok(found)
}

/// For `Ctx` (the layout shows "Account" or "Sign in").
pub async fn current_user(
    parts: &mut Parts,
    state: &AppState,
) -> Result<Option<user::Model>, AppError> {
    Ok(load(parts, state).await?.map(|(_, u)| u))
}

/// Signed out: to the sign-in page, remembering a GET's path so sign-in can come back to it.
/// htmx requests get `HX-Redirect` (a 303 would be followed inside the swap).
fn to_sign_in(parts: &Parts, state: &AppState) -> Response {
    let secure = state.config.is_production();
    let mut response = if parts.headers.contains_key("hx-request") {
        let mut r = axum::http::StatusCode::OK.into_response();
        if let Ok(v) = HeaderValue::from_str(&paths::new_session()) {
            r.headers_mut().insert("hx-redirect", v);
        }
        r
    } else {
        Redirect::to(&paths::new_session()).into_response()
    };
    if parts.method == axum::http::Method::GET {
        let path = parts
            .uri
            .path_and_query()
            .map_or_else(|| parts.uri.path().to_owned(), |pq| pq.as_str().to_owned());
        if let Some(cookie) = return_to::set(&path, secure) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    response
}

// ---------------------------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------------------------

/// The signed-in person, if any. Never rejects (database errors aside).
pub struct CurrentUser(pub Option<user::Model>);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        Ok(Self(current_user(parts, state).await?))
    }
}

/// The signed-in person, or a redirect to sign in (with return-to).
// SECURITY: put this (or a stronger extractor) on every handler that shows or changes private
// data; the security suite requests every `Access::User` route signed out and expects no 2xx
// (test: the_account_page_needs_a_session_and_remembers_where_you_were_going, non_public_routes_never_answer_2xx_anonymously)
pub struct RequireUser(pub user::Model);

impl FromRequestParts<AppState> for RequireUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        match load(parts, state).await {
            Ok(Some((_, u))) => Ok(Self(u)),
            Ok(None) => Err(to_sign_in(parts, state)),
            Err(e) => Err(e.into_response()),
        }
    }
}

/// The session itself and its person (sign-out, the sessions list, re-auth).
pub struct CurrentSession {
    pub session: session::Model,
    pub user: user::Model,
}

impl FromRequestParts<AppState> for CurrentSession {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        match load(parts, state).await {
            Ok(Some((session, user))) => Ok(Self { session, user }),
            Ok(None) => Err(to_sign_in(parts, state)),
            Err(e) => Err(e.into_response()),
        }
    }
}

// WHY: axum 0.8 gives `Option<T>` only through this trait (a sign-in page may have a session).
impl OptionalFromRequestParts<AppState> for CurrentSession {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, AppError> {
        Ok(load(parts, state)
            .await?
            .map(|(session, user)| Self { session, user }))
    }
}

/// Signed in **and** proved it recently (`REAUTH_WINDOW`). Otherwise: to `/account/confirm`,
/// which comes back here afterwards.
// SECURITY: a stolen session cookie or an unlocked laptop must not be enough to change the
// email, the password or two-factor: those take the account over for good.
// INVARIANT: sensitive actions need a recent re-auth (test: sensitive_actions_require_recent_auth)
pub struct RequireRecentAuth {
    pub session: session::Model,
    pub user: user::Model,
}

impl FromRequestParts<AppState> for RequireRecentAuth {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let (session, user) = match load(parts, state).await {
            Ok(Some(found)) => found,
            Ok(None) => return Err(to_sign_in(parts, state)),
            Err(e) => return Err(e.into_response()),
        };
        if Utc::now() - session.authenticated_at <= REAUTH_WINDOW {
            return Ok(Self { session, user });
        }
        // Come back to this page after confirming: the path of a GET, the account page after a
        // POST (a POST can't be replayed by a redirect).
        let secure = state.config.is_production();
        let back = if parts.method == axum::http::Method::GET {
            parts
                .uri
                .path_and_query()
                .map_or_else(paths::account, |pq| pq.as_str().to_owned())
        } else {
            paths::account()
        };
        let mut response = (
            Flash::notice("Please confirm it's you to continue."),
            Redirect::to(&paths::account_confirm()),
        )
            .into_response();
        if parts.headers.contains_key("hx-request") {
            response = axum::http::StatusCode::OK.into_response();
            if let Ok(v) = HeaderValue::from_str(&paths::account_confirm()) {
                response.headers_mut().insert("hx-redirect", v);
            }
        }
        if let Some(cookie) = return_to::set(&back, secure) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
        Err(response)
    }
}

/// Where a request came from, for the audit trail and the sessions list.
pub struct RequestMeta {
    pub meta: Meta,
    /// The browser's device id, when it already has one.
    pub device: Option<String>,
}

impl FromRequestParts<AppState> for RequestMeta {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ClientIp(ip) = match ClientIp::from_request_parts(parts, state).await {
            Ok(ip) => ip,
            Err(never) => match never {},
        };
        let secure = state.config.is_production();
        let device = auth_cookie::read(&parts.headers, &auth_cookie::name(DEVICE_COOKIE, secure));
        Ok(Self {
            meta: Meta {
                ip: ip.map(|ip| ip.to_string()),
                user_agent: parts
                    .headers
                    .get(header::USER_AGENT)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                device_digest: device.as_deref().map(auth_token::digest),
            },
            device,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Signing in
// ---------------------------------------------------------------------------------------------

/// Completes a sign-in once the first factor checked out. Every sign-in method calls this; a
/// second-factor sample inserts its check at the anchor and may answer with its own step.
///
/// `method` is a short label stored on the session ("password", "magic_link", "reset", …).
pub async fn sign_in(
    state: &AppState,
    headers: &HeaderMap,
    meta: &RequestMeta,
    user: user::Model,
    method: &'static str,
) -> Result<Response, AppError> {
    // samples: before sign in
    finish_sign_in(state, headers, meta, user, method).await
}

/// Starts the session: replaces any session this browser had (rotation), records the event,
/// warns the person by email about a new device, sets the cookies and goes where the person
/// was headed. Second-factor samples call this after their own check.
// SECURITY: a brand-new token on every sign-in, and the old one deleted, defeats session
// fixation (a token planted before sign-in is worthless after it).
// INVARIANT: sign-in rotates the session (test: sign_in_rotates_the_session_and_kills_the_old_token)
pub async fn finish_sign_in(
    state: &AppState,
    headers: &HeaderMap,
    meta: &RequestMeta,
    user: user::Model,
    method: &'static str,
) -> Result<Response, AppError> {
    let secure = state.config.is_production();
    let now = Utc::now();
    let device = meta
        .device
        .clone()
        .unwrap_or_else(auth_token::generate_token);
    let device_digest = auth_token::digest(&device);
    let mut audit = meta.meta.clone();
    audit.device_digest = Some(device_digest.clone());

    let txn = db::begin(&state.db).await?;
    // The session this browser had (any account's): replaced, and its owner told after commit.
    let mut replaced = None;
    if let Some(old) = auth_cookie::read_session(headers, secure) {
        replaced = session::find_by_token(&txn, &old, now)
            .await?
            .map(|(_, u)| u.id);
        session::destroy_by_token(&txn, &old).await?;
    }
    let new_device = auth_event::ever_signed_in(&txn, user.id).await?
        && !auth_event::known_device(&txn, user.id, &device_digest).await?;
    let (_, raw) = session::start(
        &txn,
        user.id,
        method,
        audit.ip.clone(),
        audit.user_agent.clone(),
        now,
    )
    .await?;
    auth_event::record(&txn, Some(user.id), Kind::SignedIn, &audit, now).await?;
    txn.commit().await?;
    // SECURITY: the replaced session's sockets and streams close now, not at their next re-check
    // (a shared computer: Ann signs in over Bob's session, Bob's live room must not stay open)
    // (test: every_session_ending_path_calls_sessions_ended, signing_in_over_a_session_closes_its_sockets)
    if let Some(old_user) = replaced {
        sessions_ended(state, old_user);
    }

    if new_device {
        notify(
            state,
            user.id,
            Notice::NewSignIn {
                device: crate::kernel::user_agent::describe(audit.user_agent.as_deref()),
                ip: audit.ip.clone(),
            },
        );
    }

    // APP: where people land after signing in (unless they were headed somewhere).
    let destination = return_to::read(headers, secure).unwrap_or_else(paths::after_sign_in);
    let mut response = (
        Flash::notice(format!("Signed in as {}.", user.email)),
        Redirect::to(&destination),
    )
        .into_response();
    let h = response.headers_mut();
    h.append(header::SET_COOKIE, auth_cookie::set_session(&raw, secure));
    h.append(header::SET_COOKIE, return_to::clear(secure));
    if meta.device.is_none() {
        h.append(
            header::SET_COOKIE,
            auth_cookie::set(
                &auth_cookie::name(DEVICE_COOKIE, secure),
                &device,
                Some(DEVICE_MAX_AGE_SECS),
                secure,
            ),
        );
    }
    Ok(response)
}

/// `true` when re-authentication must use a second factor (two-factor is on). Password and
/// emailed-code re-auth refuse then, so a phished password can't turn two-factor off.
// SECURITY: without this, step-up with the password alone could disable the second factor.
// INVARIANT: with two-factor on, password re-auth is refused (test: two_factor_users_must_step_up_with_a_code)
pub async fn step_up_needs_second_factor(
    state: &AppState,
    user: &user::Model,
) -> Result<bool, AppError> {
    #[allow(unused_mut)] // second-factor samples wire their check below
    let mut needed = false;
    // samples: step-up second factor
    let _ = (state, user);
    Ok(needed)
}

/// Completes a re-authentication: rotates the session token (privilege change), records it and
/// goes back to where the person was.
pub async fn grant_step_up(
    state: &AppState,
    headers: &HeaderMap,
    meta: &RequestMeta,
    current: &CurrentSession,
) -> Result<Response, AppError> {
    let secure = state.config.is_production();
    let now = Utc::now();
    let txn = db::begin(&state.db).await?;
    let raw = session::rotate(&txn, current.session.id, now).await?;
    auth_event::record(
        &txn,
        Some(current.user.id),
        Kind::Reauthenticated,
        &meta.meta,
        now,
    )
    .await?;
    txn.commit().await?;
    let destination = return_to::read(headers, secure).unwrap_or_else(paths::account);
    let mut response = (
        Flash::notice("Thanks, you're confirmed."),
        Redirect::to(&destination),
    )
        .into_response();
    let h = response.headers_mut();
    h.append(header::SET_COOKIE, auth_cookie::set_session(&raw, secure));
    h.append(header::SET_COOKIE, return_to::clear(secure));
    Ok(response)
}

/// Signs the person out everywhere and drops anything else that keeps them signed in (trusted
/// devices, pending sign-ins). Password change and reset, "this wasn't me". Run in the
/// caller's transaction.
// SECURITY: a password change must end every other session, or the attacker who caused it stays in.
// INVARIANT: credential changes end every session (test: not_me_signs_out_everywhere)
pub async fn revoke_everything(
    txn: &impl sea_orm::ConnectionTrait,
    user_id: i64,
) -> Result<(), AppError> {
    session::revoke_all(txn, user_id).await?;
    // SECURITY: a pending email change (a link to an attacker's address) dies too, or the
    // attacker who started it still moves the account after the owner secured it
    // (test: not_me_signs_out_everywhere, a_password_reset_cancels_a_pending_email_change)
    user_token::revoke_all(txn, user_id, user_token::Purpose::EmailChange).await?;
    // samples: revoke everything
    Ok(())
}

/// Like `revoke_everything`, but keeps the current session (a password change from the account
/// page: this device stays signed in, every other one is signed out).
pub async fn revoke_other_access(
    txn: &impl sea_orm::ConnectionTrait,
    user: &user::Model,
    keep_session_id: i64,
) -> Result<(), AppError> {
    session::revoke_others(txn, user, keep_session_id).await?;
    // SECURITY: a pending email change started elsewhere dies with the password change
    // (test: a_password_change_cancels_a_pending_email_change)
    user_token::revoke_all(txn, user.id, user_token::Purpose::EmailChange).await?;
    // samples: revoke other access
    Ok(())
}

/// The person who just proved they read the address (a magic link or an emailed code) verifies
/// the account, and if it was **unverified**, takes it over: everything set up on it before that
/// proof is dropped, because an attacker may have registered the address first, with a password
/// the attacker knows, hoping the owner would sign in some other way and verify it for them
/// (account pre-hijacking). Sessions, emailed tokens and every credential a sign-in sample keeps
/// (its wire at the anchor) go. Returns `true` when it claimed an unverified account: call
/// `sessions_ended` after the commit then. Run it in the caller's transaction.
// SECURITY: without it, the attacker's password still opens the account the owner just verified
// (test: a_password_set_before_verification_dies_when_the_owner_signs_in_by_link)
// WHY: verify first: the UPDATE locks the row, so a sign-up racing to set a password either
// committed before (and its password is deleted below) or waits and finds the account verified.
pub async fn claim_unverified_account(
    txn: &impl sea_orm::ConnectionTrait,
    user_id: i64,
    now: chrono::DateTime<Utc>,
) -> Result<bool, AppError> {
    if !user::mark_verified(txn, user_id, now).await? {
        return Ok(false);
    }
    session::revoke_all(txn, user_id).await?;
    user_token::revoke_every_purpose(txn, user_id).await?;
    // samples: claim unverified account
    Ok(true)
}

/// Sessions of `user_id` ended: call it after the commit, wherever that happens (sign-out, a
/// revoked device, "sign out everywhere else", password change and reset, "this wasn't me",
/// turning two-factor on). Whatever keeps a live connection for that person re-checks its session
/// here: the realtime sample wires `channels::session_ended` in, so open sockets close at once.
// INVARIANT: every place that deletes session rows calls this after its commit
// (test: every_session_ending_path_calls_sessions_ended)
pub fn sessions_ended(state: &AppState, user_id: i64) {
    let _ = (state, user_id);
    // samples: sessions ended
}

/// The `auth_throttles` key for guesses at `subject` in `scope` ("login", "reauth", "totp"):
/// an HMAC, so the table holds no email addresses.
pub fn throttle_key(state: &AppState, scope: &str, subject: &str) -> String {
    auth_token::keyed_digest(
        &state.config.auth.secret_key,
        "throttle",
        &format!("{scope}:{subject}"),
    )
}

// ---------------------------------------------------------------------------------------------
// Security notices
// ---------------------------------------------------------------------------------------------

/// Emails the person about a security event, in the background, with a "this wasn't me" link
/// minted inside the task. Goes to the account's current address unless the notice says
/// otherwise (an email change also warns the old address).
// SECURITY: the token exists only in the email and as a digest; never in a log or job row
// (test: session_tokens_never_reach_the_logs)
pub fn notify(state: &AppState, user_id: i64, notice: Notice) {
    let state = state.clone();
    state
        .clone()
        .background
        .spawn("security notice email", async move {
            let user = user::find_self(&state.db, user_id).await?;
            let to = notice.recipient().unwrap_or(&user.email).to_owned();
            let token = user_token::issue(
                &state.db,
                user.id,
                user_token::Purpose::NotMe,
                &to,
                None,
                Utc::now(),
            )
            .await?;
            let email = auth_mailer::notice(&state.config, &to, &notice, &token)?;
            state.mailer.deliver(email).await?;
            Ok(())
        });
}
