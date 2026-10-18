//! Sign-up and email verification. Sign-up is public under `Signup::Open` (`config.auth.signup`)
//! and 404 under `Signup::Invite`; verification works in both (an account made before the switch,
//! or a resent link).
//!
//! GUIDE: .offrails/guides/auth.md#sign-up-signup-only-registrationsrs-new-create
//!
//! Sign-up never says whether an address already has an account. Every submit answers "check
//! your email"; the mailbox owner then gets either a verification link (new address) or a
//! "you already have an account" note (existing address). Nobody is signed in until the address
//! is verified.
//!
//! Pre-hijacking: an attacker could sign up with someone else's address and a password they
//! know, hoping the owner clicks the verification link. Two defenses:
//! 1. the latest sign-up for an unverified address wins (its password replaces the earlier one,
//!    older links die), so the real owner signing up takes the address back;
//! 2. the link signs in with one click only in the browser that signed up (a cookie); anywhere
//!    else it asks for the password chosen at sign-up, which a victim clicking an attacker's
//!    link doesn't know.

use std::time::Duration as StdDuration;

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::{
        account::{invalid_link, token_page},
        application::ClientIp,
        auth::{self, CurrentUser, RequestMeta, SignupOpen},
    },
    error::AppError,
    kernel::{auth_token, db, session as auth_cookie},
    mailers::auth_mailer::{AuthMail, url},
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
        passwords::{RegistrationNew, VerificationShow},
    },
};

const SIGNUP_COOKIE: &str = "signup";
// APP: how long the sign-up browser stays recognized (match the verification link's lifetime).
const SIGNUP_COOKIE_SECS: i64 = 24 * 60 * 60;

// APP: sign-up limits: per network, and per address (each submit can send an email).
const IP_LIMIT: u32 = 10;
const IP_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);
const EMAIL_LIMIT: u32 = 3;
const EMAIL_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);

/// Untrusted input from the sign-up form. No `Debug` derive: the password must never be logged.
#[derive(Default, Deserialize)]
pub struct SignupParams {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
}

/// `GET /registration/new`. Every sign-up handler takes `SignupOpen`: under `Signup::Invite`
/// they answer 404 (auth-core's `auth_invite` creates the accounts then).
pub async fn new(
    _: SignupOpen,
    ctx: Ctx,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    if user.is_some() {
        return Ok(Redirect::to(&paths::after_sign_in()).into_response());
    }
    Ok(views::render(RegistrationNew {
        ctx: ctx.titled("Create your account"),
        email: String::new(),
        errors: Errors::default(),
        alert: None,
        strength: None,
    })?
    .into_response())
}

/// `POST /registration`
// SECURITY: the response (status, location, cookies) is the same for new and existing
// addresses; only the email differs, and only the mailbox owner reads it.
// INVARIANT: sign-up doesn't reveal existing accounts (test: signup_with_an_existing_email_looks_identical)
pub async fn create(
    _: SignupOpen,
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    ip: ClientIp,
    Form(params): Form<SignupParams>,
) -> Result<Response, AppError> {
    let ctx = ctx.titled("Create your account");
    let render_form = |ctx: Ctx,
                       status: StatusCode,
                       errors: Errors,
                       alert: Option<String>|
     -> Result<Response, AppError> {
        let strength = (!params.password.is_empty())
            .then(|| password_policy::strength(&params.password, &params.email));
        Ok((
            status,
            views::render(RegistrationNew {
                ctx,
                email: params.email.clone(),
                errors,
                alert,
                strength,
            })?,
        )
            .into_response())
    };

    let mut errors = Errors::default();
    let email = match user::validate_email(&params.email) {
        Ok(e) => Some(e),
        Err(e) => {
            errors = e;
            None
        }
    };
    if let Some(problem) = password_policy::problem(&params.password, &params.email) {
        errors.add("password", problem);
    }
    let Some(email) = email.filter(|_| errors.is_empty()) else {
        return render_form(ctx, StatusCode::UNPROCESSABLE_ENTITY, errors, None);
    };
    if s.rate_limit
        .check(&ip.key("registration#create"), IP_LIMIT, IP_WINDOW)
        .await
        .is_err()
        || s.rate_limit
            .check(
                &format!("registration#create:{email}"),
                EMAIL_LIMIT,
                EMAIL_WINDOW,
            )
            .await
            .is_err()
    {
        return render_form(
            ctx,
            StatusCode::TOO_MANY_REQUESTS,
            Errors::default(),
            Some("Too many sign-up attempts. Try again later.".to_owned()),
        );
    }

    // The hash is computed for every submit, existing address or not (same cost either way).
    let digest = password_credential::hash(params.password.clone()).await?;
    let browser = auth_token::generate_token();
    let now = Utc::now();

    let txn = db::begin(&s.db).await?;
    let existing = user::find_by_email(&txn, &email).await?;
    let pending = match &existing {
        // A verified account (or a passwordless one): tell the owner, change nothing.
        Some(u) if u.is_verified() => None,
        // An unverified sign-up: the latest one wins (defense 1), unless the owner verifies it
        // meanwhile (a magic link): the row lock orders the two.
        // SECURITY: a sign-up racing the owner's verification can't leave its password behind
        // (test: a_password_set_before_verification_dies_when_the_owner_signs_in_by_link)
        Some(u) if user::lock_unverified(&txn, u.id, now).await? => Some(u.clone()),
        Some(_) => None,
        None => {
            let u = user::find_or_create(&txn, &email, now).await?;
            auth_event::record(&txn, Some(u.id), Kind::SignedUp, &meta.meta, now).await?;
            Some(u)
        }
    };
    if let Some(u) = &pending {
        password_credential::set(&txn, u.id, digest, now).await?;
    }
    txn.commit().await?;

    match pending {
        Some(u) => send_verification(&s, &u, Some(auth_token::digest(&browser))),
        None => {
            if let Some(u) = existing {
                send_already_registered(&s, &u);
            }
        }
    }

    let secure = s.config.is_production();
    let mut response = Redirect::to(&paths::registration_sent()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set(
            &auth_cookie::name(SIGNUP_COOKIE, secure),
            &browser,
            Some(SIGNUP_COOKIE_SECS),
            secure,
        ),
    );
    Ok(response)
}

/// `GET /registration/sent`
pub async fn sent(_: SignupOpen, ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(CheckEmail {
        ctx: ctx.titled("Check your email").noindex(),
        heading: "Check your email".to_owned(),
        // APP: wording. It must read the same whether or not the address already had an account.
        message: "We've sent a message to the address you entered. Open it on this device to finish creating your account. The link works for 24 hours.".to_owned(),
        code_form: None,
    })?
    .into_response())
}

/// Mints the verification token in the background and emails the link.
pub fn send_verification(s: &AppState, user: &user::Model, browser_digest: Option<String>) {
    let state = s.clone();
    let (user_id, to) = (user.id, user.email.clone());
    s.background.spawn("verification email", async move {
        let token = user_token::issue(
            &state.db,
            user_id,
            user_token::Purpose::EmailVerification,
            &to,
            browser_digest,
            Utc::now(),
        )
        .await?;
        let app = crate::config::APP_NAME;
        let email = AuthMail {
            to: to.clone(),
            subject: format!("Confirm your email for {app}"),
            heading: "Confirm your email address".to_owned(),
            intro: vec![format!("Thanks for signing up for {app}. Confirm this is your address to finish creating your account.")],
            action: Some(("Confirm my email".to_owned(), url(&state.config, &paths::email_verification(&token)))),
            outro: vec![
                "The link works once, for 24 hours.".to_owned(),
                "If you didn't sign up, ignore this email: no account is created without this confirmation.".to_owned(),
            ],
            ..Default::default()
        }
        .render()?;
        state.mailer.deliver(email).await?;
        Ok(())
    });
}

/// A new verification link for an unverified account (asked for by signing in), at most a few
/// per hour.
pub fn resend_verification(s: &AppState, user: &user::Model) {
    let state = s.clone();
    let user = user.clone();
    s.background.spawn("verification resend", async move {
        let key = format!("verification#resend:{}", user.id);
        if state
            .rate_limit
            .check(&key, EMAIL_LIMIT, EMAIL_WINDOW)
            .await
            .is_ok()
        {
            send_verification(&state, &user, None);
        }
        Ok(())
    });
}

/// "You already have an account": the only thing an existing address hears about a sign-up.
fn send_already_registered(s: &AppState, user: &user::Model) {
    let state = s.clone();
    let to = user.email.clone();
    s.background.spawn("already registered email", async move {
        let app = crate::config::APP_NAME;
        let email = AuthMail {
            to: to.clone(),
            subject: format!("You already have a {app} account"),
            heading: "You already have an account".to_owned(),
            intro: vec![format!("Someone tried to create a {app} account with this address. You already have one, so nothing changed.")],
            action: Some(("Sign in".to_owned(), url(&state.config, &paths::new_session()))),
            outro: vec![
                format!("Forgot your password? Reset it here: {}", url(&state.config, &paths::new_password())),
                "If it wasn't you, you can ignore this email.".to_owned(),
            ],
            ..Default::default()
        }
        .render()?;
        state.mailer.deliver(email).await?;
        Ok(())
    });
}

/// `GET /email-verification/{token}`: a confirm button (one click in the sign-up browser,
/// otherwise the sign-up password). GET never consumes the token (link scanners).
pub async fn verification(
    State(s): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_session()).noindex();
    match user_token::find_valid(
        &s.db,
        user_token::Purpose::EmailVerification,
        &token,
        Utc::now(),
    )
    .await?
    {
        Some((t, u)) => Ok(token_page(
            views::render(VerificationShow {
                ctx: ctx.titled("Confirm your email"),
                email: u.email,
                action: paths::email_verification(&token),
                needs_password: !same_browser(&s, &headers, &t),
                errors: Errors::default(),
                alert: None,
            })?
            .into_response(),
        )),
        None => invalid(ctx),
    }
}

#[derive(Default, Deserialize)]
pub struct VerifyParams {
    #[serde(default)]
    pub password: String,
}

/// `POST /email-verification/{token}`: verifies the address and signs in.
// SECURITY: outside the sign-up browser the sign-up password is required (defense 2 above),
// checked under the same throttle as sign-in.
// INVARIANT: a link opened elsewhere needs the password (test: verification_in_another_browser_needs_the_password)
pub async fn verify(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(params): Form<VerifyParams>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_session()).noindex();
    let now = Utc::now();
    let Some((t, u)) =
        user_token::find_valid(&s.db, user_token::Purpose::EmailVerification, &token, now).await?
    else {
        return invalid(ctx);
    };
    if !same_browser(&s, &headers, &t) {
        let retry = |ctx: Ctx,
                     status: StatusCode,
                     errors: Errors,
                     alert: Option<String>|
         -> Result<Response, AppError> {
            Ok(token_page(
                (
                    status,
                    views::render(VerificationShow {
                        ctx: ctx.titled("Confirm your email"),
                        email: u.email.clone(),
                        action: paths::email_verification(&token),
                        needs_password: true,
                        errors,
                        alert,
                    })?,
                )
                    .into_response(),
            ))
        };
        let key = auth::throttle_key(&s, "login", &u.email);
        let attempt = match auth_throttle::begin(&s.db, &key, now).await? {
            Ok(a) => a,
            Err(paused) => {
                return retry(
                    ctx,
                    StatusCode::TOO_MANY_REQUESTS,
                    Errors::default(),
                    Some(format!(
                        "Too many wrong passwords. Try again in {}.",
                        paused.wait_text(now)
                    )),
                );
            }
        };
        let checked = password_credential::check(&s.db, Some(u.id), &params.password).await?;
        if !checked.ok {
            auth_event::record(&s.db, Some(u.id), Kind::SignInFailed, &meta.meta, now).await?;
            return retry(
                ctx,
                StatusCode::UNPROCESSABLE_ENTITY,
                Errors::single(
                    "password",
                    "doesn't match the one you chose when signing up",
                ),
                None,
            );
        }
        auth_throttle::succeeded(&s.db, attempt).await?;
    }

    let txn = db::begin(&s.db).await?;
    let Some((_, u)) =
        user_token::consume(&txn, user_token::Purpose::EmailVerification, &token, now).await?
    else {
        txn.rollback().await?;
        return invalid(ctx);
    };
    user::mark_verified(&txn, u.id, now).await?;
    auth_event::record(&txn, Some(u.id), Kind::EmailVerified, &meta.meta, now).await?;
    txn.commit().await?;
    let u = user::find_self(&s.db, u.id).await?;
    let mut response = auth::sign_in(&s, &headers, &meta, u, "verification").await?;
    let secure = s.config.is_production();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::clear(&auth_cookie::name(SIGNUP_COOKIE, secure), secure),
    );
    Ok(response)
}

/// The token was bound to a browser, and this request carries that browser's cookie.
fn same_browser(s: &AppState, headers: &HeaderMap, token: &user_token::Model) -> bool {
    let Some(expected) = token.browser_digest.as_deref() else {
        return false;
    };
    let secure = s.config.is_production();
    auth_cookie::read(headers, &auth_cookie::name(SIGNUP_COOKIE, secure)).is_some_and(|raw| {
        // SECURITY: constant-time, so the comparison doesn't leak the digest byte by byte
        // (test: verification_in_another_browser_needs_the_password)
        auth_token::constant_time_eq(auth_token::digest(&raw).as_bytes(), expected.as_bytes())
    })
}

fn invalid(ctx: Ctx) -> Result<Response, AppError> {
    invalid_link(
        ctx,
        "This confirmation link doesn't work",
        "Confirmation links work once and expire after 24 hours. Sign in with your email and password and we'll send a fresh one.",
        "Go to sign in",
        &paths::new_session(),
    )
}
