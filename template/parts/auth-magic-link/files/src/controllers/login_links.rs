//! Passwordless sign-in: ask for a link, then click it or type its 6-digit code.
//!
//! GUIDE: .offrails/guides/auth.md#magic-link-variants-magic-link-magic-link-invite
//!
//! | Step | Route |
//! |------|-------|
//! | Ask for a link | `POST /login_links` → 303 `/login_links/sent` (same answer for every address) |
//! | Type the code | `POST /login_links/code` (only in the browser that asked) |
//! | Open the link | `GET /login_links/{token}`: a confirm button; GET never signs in (link scanners) |
//! | Use the link | `POST /login_links/{token}` |
//! | Re-auth by email | `POST /account/confirm/email`, then `POST /account/confirm/email/code` |

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
        account::{self, invalid_link, token_page},
        application::ClientIp,
        auth::{self, CurrentSession, RequestMeta},
    },
    error::AppError,
    kernel::{auth_token, db, flash::Flash, session as auth_cookie},
    mailers::auth_mailer::{AuthMail, url},
    models::{
        Errors,
        auth_event::{self, Kind},
        auth_throttle,
        login_link::{self, CodeCheck, Purpose},
        user,
    },
    routes::paths,
    state::AppState,
    views::{
        self, Ctx,
        auth::{CheckEmail, CodeForm, CtxExt, SessionsNew},
        login_links::{LoginLinkOtherBrowser, LoginLinkShow},
    },
};

/// The cookie that ties a link and its code to the browser that asked for them.
const BROWSER_COOKIE: &str = "login_browser";
const BROWSER_COOKIE_SECS: i64 = 20 * 60;

// APP: request limits: 10 per 3 minutes per network and per address (each sends an email).
const LIMIT: u32 = 10;
const WINDOW: StdDuration = StdDuration::from_secs(3 * 60);
// APP: links per address per day. Each one emails the address, and each one's code can be guessed.
pub const DAILY_LIMIT: u32 = 20;
const DAY: StdDuration = StdDuration::from_secs(24 * 60 * 60);

/// The rate-limit key of the daily cap for an address.
pub fn daily_key(email: &str) -> String {
    format!("login_links#day:{email}")
}

/// The `auth_throttles` key for wrong codes sent to `email`, whichever link they were for.
pub fn code_throttle_key(s: &AppState, email: &str) -> String {
    auth::throttle_key(s, "magic_code", email)
}

#[derive(Debug, Default, Deserialize)]
pub struct LinkParams {
    #[serde(default)]
    pub email: String,
}

/// `POST /login_links`: emails a link and code. Same redirect for every valid address.
// SECURITY: no hint whether the address has an account (both get an email under `magic-link`,
// where the first use creates the account); rate limited per network and per address.
// INVARIANT: known and unknown addresses get identical responses (test: known_and_unknown_addresses_get_identical_responses)
pub async fn create(
    State(s): State<AppState>,
    ctx: Ctx,
    ip: ClientIp,
    Form(params): Form<LinkParams>,
) -> Result<Response, AppError> {
    let ctx = ctx.titled("Sign in");
    let form = |ctx: Ctx,
                status: StatusCode,
                errors: Errors,
                alert: Option<String>|
     -> Result<Response, AppError> {
        let page = SessionsNew {
            ctx,
            email: params.email.clone(),
            alert,
            errors,
            form: "magic_link",
        };
        Ok((status, views::render(page)?).into_response())
    };
    let email = match user::validate_email(&params.email) {
        Ok(e) => e,
        Err(errors) => return form(ctx, StatusCode::UNPROCESSABLE_ENTITY, errors, None),
    };
    if s.rate_limit
        .check(&ip.key("login_links#create"), LIMIT, WINDOW)
        .await
        .is_err()
        || s.rate_limit
            .check(&format!("login_links#create:{email}"), LIMIT, WINDOW)
            .await
            .is_err()
        // SECURITY: a daily cap per address bounds the mail flood and the links (each with its
        // code) a day of requests buys (test: link_requests_are_capped_per_address_per_day)
        || s.rate_limit
            .check(&daily_key(&email), DAILY_LIMIT, DAY)
            .await
            .is_err()
    {
        let mut r = form(
            ctx,
            StatusCode::TOO_MANY_REQUESTS,
            Errors::default(),
            Some("Too many sign-in links requested. Try again in a few minutes.".to_owned()),
        )?;
        r.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("180"));
        return Ok(r);
    }

    // A fresh browser cookie per request: the newest link wins in this browser.
    let secure = s.config.is_production();
    let browser = auth_token::generate_token();
    // APP: invite-only sign-in: only send when `user::find_by_email` finds an account (README).
    send_link(&s, &email, Purpose::SignIn, auth_token::digest(&browser));
    let mut response = Redirect::to(&paths::login_links_sent()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set(
            &auth_cookie::name(BROWSER_COOKIE, secure),
            &browser,
            Some(BROWSER_COOKIE_SECS),
            secure,
        ),
    );
    Ok(response)
}

/// Mints the link and code in the background and emails them.
fn send_link(s: &AppState, email: &str, purpose: Purpose, browser_digest: String) {
    let state = s.clone();
    let to = email.to_owned();
    s.background.spawn("sign-in link email", async move {
        let minted = login_link::mint(
            &state.db,
            &state.config.auth.secret_key,
            &to,
            purpose,
            &browser_digest,
            Utc::now(),
        )
        .await?;
        let app = crate::config::APP_NAME;
        let mail = match purpose {
            Purpose::SignIn => AuthMail {
                to: to.clone(),
                subject: format!("Your {app} sign-in link"),
                heading: format!("Sign in to {app}"),
                intro: vec!["Click the button to sign in. If this is your first time, it creates your account.".to_owned()],
                action: Some(("Sign in".to_owned(), url(&state.config, &paths::login_link(&minted.token)))),
                code: Some(minted.code),
                outro: vec![
                    "Or type the code above on the page where you asked for it. The link and code work once, for 15 minutes.".to_owned(),
                    "If you didn't ask to sign in, ignore this email: nobody can use it without access to your inbox.".to_owned(),
                ],
                ..Default::default()
            },
            Purpose::Reauth => AuthMail {
                to: to.clone(),
                subject: format!("Your {app} confirmation code"),
                heading: "Confirm it's you".to_owned(),
                intro: vec!["Type this code on the page that asked for it to continue.".to_owned()],
                code: Some(minted.code),
                outro: vec!["It works once, for 15 minutes. If you didn't ask for it, someone may be using your account: sign out every other device from your account page.".to_owned()],
                ..Default::default()
            },
        };
        state.mailer.deliver(mail.render()?).await?;
        Ok(())
    });
}

/// `GET /login_links/sent`: "check your email", with the code form.
pub async fn sent(ctx: Ctx) -> Result<Response, AppError> {
    check_email_page(ctx, StatusCode::OK, None)
}

fn check_email_page(
    ctx: Ctx,
    status: StatusCode,
    error: Option<String>,
) -> Result<Response, AppError> {
    let page = CheckEmail {
        ctx: ctx.titled("Check your email").noindex(),
        heading: "Check your email".to_owned(),
        // APP: wording. It must read the same whether or not the address has an account.
        message: "We've sent you a sign-in link and a 6-digit code. Click the link, or type the code below. Both work once, for 15 minutes.".to_owned(),
        code_form: Some(CodeForm { action: paths::login_links_code(), error }),
    };
    Ok((status, views::render(page)?).into_response())
}

#[derive(Default, Deserialize)]
pub struct CodeParams {
    #[serde(default)]
    pub code: String,
}

/// `POST /login_links/code`: signs in with the code, in the browser that asked for it.
pub async fn code(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    headers: HeaderMap,
    ip: ClientIp,
    Form(params): Form<CodeParams>,
) -> Result<Response, AppError> {
    let secure = s.config.is_production();
    let Some(browser) = auth_cookie::read(&headers, &auth_cookie::name(BROWSER_COOKIE, secure))
    else {
        return check_email_page(
            ctx,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("That code has expired. Ask for a new sign-in link.".to_owned()),
        );
    };
    // SECURITY: a per-network cap on code guesses, on top of the per-link attempt limit
    // (test: code_guesses_are_capped_per_network)
    if s.rate_limit
        .check(&ip.key("login_links#code"), 30, WINDOW)
        .await
        .is_err()
    {
        return check_email_page(
            ctx,
            StatusCode::TOO_MANY_REQUESTS,
            Some("Too many attempts. Try again in a few minutes.".to_owned()),
        );
    }
    let now = Utc::now();
    let Some(target) =
        login_link::newest_for_browser(&s.db, Purpose::SignIn, &browser, now).await?
    else {
        return code_expired(ctx);
    };
    // SECURITY: wrong codes count per address across every link, in the database, with growing
    // pauses: a new link (or another browser, another network) doesn't buy new guesses
    // (test: code_guesses_are_throttled_per_address_across_links)
    let attempt =
        match auth_throttle::begin(&s.db, &code_throttle_key(&s, &target.email), now).await? {
            Ok(attempt) => attempt,
            Err(paused) => return code_paused(ctx, paused, now),
        };
    let link = match login_link::check_code(&s.db, &s.config.auth.secret_key, target, &params.code)
        .await?
    {
        CodeCheck::Valid(link) => {
            auth_throttle::succeeded(&s.db, attempt).await?;
            link
        }
        CodeCheck::Wrong { left } => {
            auth_event::record(&s.db, None, Kind::SignInFailed, &meta.meta, now).await?;
            let message = if left == 1 {
                "That code isn't right. 1 try left.".to_owned()
            } else {
                format!("That code isn't right. {left} tries left.")
            };
            return check_email_page(ctx, StatusCode::UNPROCESSABLE_ENTITY, Some(message));
        }
        CodeCheck::NoLink => return code_expired(ctx),
    };
    finish(&s, ctx, &meta, &headers, link, "magic_link_code").await
}

fn code_expired(ctx: Ctx) -> Result<Response, AppError> {
    check_email_page(
        ctx,
        StatusCode::UNPROCESSABLE_ENTITY,
        Some("That code has expired or was used up. Ask for a new sign-in link.".to_owned()),
    )
}

/// Too many wrong codes for this address lately: a 429 that says when to come back.
fn code_paused(
    ctx: Ctx,
    paused: auth_throttle::Paused,
    now: chrono::DateTime<Utc>,
) -> Result<Response, AppError> {
    let mut response = check_email_page(
        ctx,
        StatusCode::TOO_MANY_REQUESTS,
        Some(format!(
            "Too many wrong codes for this address. Try again in {}, or click the link in the email.",
            paused.wait_text(now)
        )),
    )?;
    if let Ok(v) = HeaderValue::from_str(&(paused.until - now).num_seconds().max(1).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, v);
    }
    Ok(response)
}

/// `GET /login_links/{token}`: the page behind the emailed link. Never signs in.
// SECURITY: GET is side-effect free, so mail scanners and link previews can't burn the link.
// INVARIANT: opening the link never signs in (test: opening_a_link_twice_then_posting_still_signs_in)
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_session()).noindex();
    let Some(link) = login_link::find_by_token(&s.db, &token, Utc::now()).await? else {
        return invalid(ctx);
    };
    if !browser_ok(&s, &headers, &link) {
        return other_browser(ctx);
    }
    Ok(token_page(
        views::render(LoginLinkShow {
            ctx: ctx.titled("Sign in"),
            email: link.email,
            action: paths::login_link(&token),
        })?
        .into_response(),
    ))
}

/// `POST /login_links/{token}`: uses the link and signs in (creating the account on first use).
pub async fn consume(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::new_session()).noindex();
    let Some(link) = login_link::find_by_token(&s.db, &token, Utc::now()).await? else {
        return invalid(ctx);
    };
    // SECURITY: by default a link only works in the browser that asked for it, which stops
    // login CSRF (an attacker sending you a link to *their* account) and forwarded links.
    // INVARIANT: a link from another browser is refused by default (test: a_link_opened_in_another_browser_is_refused_by_default)
    if !browser_ok(&s, &headers, &link) {
        return other_browser(ctx);
    }
    finish(&s, ctx, &meta, &headers, link, "magic_link").await
}

/// Claims the link, finds or creates the account, marks the address verified and signs in.
async fn finish(
    s: &AppState,
    ctx: Ctx,
    meta: &RequestMeta,
    headers: &HeaderMap,
    link: login_link::Model,
    method: &'static str,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let txn = db::begin(&s.db).await?;
    if !login_link::claim(&txn, &link).await? {
        txn.rollback().await?;
        return invalid(ctx.at(&paths::new_session()).noindex());
    }
    // WHY: ON CONFLICT DO NOTHING + select: two first sign-ins racing can't abort the transaction.
    let existing = user::find_by_email(&txn, &link.email).await?;
    let u = user::find_or_create(&txn, &link.email, now).await?;
    if existing.is_none() {
        auth_event::record(&txn, Some(u.id), Kind::SignedUp, &meta.meta, now).await?;
    }
    // Reading the email proves the address. An account someone registered for it without that
    // proof (a password sign-up never verified) is the reader's now, minus whatever was set on it.
    // SECURITY: account pre-hijacking: the attacker's password dies here
    // (test: a_password_set_before_verification_dies_when_the_owner_signs_in_by_link)
    let claimed = auth::claim_unverified_account(&txn, u.id, now).await?;
    txn.commit().await?;
    if claimed && existing.is_some() {
        auth::sessions_ended(s, u.id);
    }
    let u = user::find_self(&s.db, u.id).await?;
    let mut response = auth::sign_in(s, headers, meta, u, method).await?;
    let secure = s.config.is_production();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::clear(&auth_cookie::name(BROWSER_COOKIE, secure), secure),
    );
    Ok(response)
}

fn browser_ok(s: &AppState, headers: &HeaderMap, link: &login_link::Model) -> bool {
    let secure = s.config.is_production();
    s.config.magic_link.any_browser
        || login_link::same_browser(
            link,
            auth_cookie::read(headers, &auth_cookie::name(BROWSER_COOKIE, secure)).as_deref(),
        )
}

fn other_browser(ctx: Ctx) -> Result<Response, AppError> {
    Ok(token_page(
        (
            StatusCode::FORBIDDEN,
            views::render(LoginLinkOtherBrowser {
                ctx: ctx.titled("Open this link where you asked for it"),
            })?,
        )
            .into_response(),
    ))
}

fn invalid(ctx: Ctx) -> Result<Response, AppError> {
    invalid_link(
        ctx,
        "This sign-in link doesn't work",
        "Sign-in links work once and expire after 15 minutes, and asking for a new one cancels the old one. Use the newest email, or ask for a fresh link.",
        "Get a new link",
        &paths::new_session(),
    )
}

// ---------------------------------------------------------------------------------------------
// Re-authentication by emailed code
// ---------------------------------------------------------------------------------------------

/// `POST /account/confirm/email`: emails a 6-digit code to the signed-in person.
pub async fn send_reauth_code(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
) -> Result<Response, AppError> {
    if auth::step_up_needs_second_factor(&s, &current.user).await? {
        return account::confirm_error(
            &s,
            ctx,
            &current,
            "magic_link",
            StatusCode::UNPROCESSABLE_ENTITY,
            Some(
                "Two-factor authentication is on: confirm with a code from your authenticator app."
                    .to_owned(),
            ),
            Errors::default(),
        )
        .await;
    }
    let key = format!("reauth#email:{}", current.user.id);
    if s.rate_limit
        .check(&key, 5, StdDuration::from_secs(15 * 60))
        .await
        .is_err()
    {
        return account::confirm_error(
            &s,
            ctx,
            &current,
            "magic_link",
            StatusCode::TOO_MANY_REQUESTS,
            Some("Too many codes requested. Try again in a few minutes.".to_owned()),
            Errors::default(),
        )
        .await;
    }
    let secure = s.config.is_production();
    let browser = auth_token::generate_token();
    send_link(
        &s,
        &current.user.email,
        Purpose::Reauth,
        auth_token::digest(&browser),
    );
    let mut response = (
        Flash::notice(format!(
            "We emailed a 6-digit code to {}.",
            current.user.email
        )),
        Redirect::to(&paths::account_confirm()),
    )
        .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth_cookie::set(
            &auth_cookie::name(BROWSER_COOKIE, secure),
            &browser,
            Some(BROWSER_COOKIE_SECS),
            secure,
        ),
    );
    Ok(response)
}

/// `POST /account/confirm/email/code`: confirms identity with the emailed code.
pub async fn confirm_reauth_code(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
    meta: RequestMeta,
    headers: HeaderMap,
    Form(params): Form<CodeParams>,
) -> Result<Response, AppError> {
    let wrong = |message: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Errors::single("code", &message),
        )
    };
    if auth::step_up_needs_second_factor(&s, &current.user).await? {
        let (status, errors) =
            wrong("can't be used: confirm with your authenticator app".to_owned());
        return account::confirm_error(&s, ctx, &current, "magic_link", status, None, errors).await;
    }
    let secure = s.config.is_production();
    let browser =
        auth_cookie::read(&headers, &auth_cookie::name(BROWSER_COOKIE, secure)).unwrap_or_default();
    let now = Utc::now();
    let Some(target) =
        login_link::newest_for_browser(&s.db, Purpose::Reauth, &browser, now).await?
    else {
        let (status, errors) = wrong("has expired; ask for a new one".to_owned());
        return account::confirm_error(&s, ctx, &current, "magic_link", status, None, errors).await;
    };
    // SECURITY: re-auth codes share the address's wrong-code throttle with sign-in codes
    // (test: code_guesses_are_throttled_per_address_across_links)
    let attempt = match auth_throttle::begin(
        &s.db,
        &code_throttle_key(&s, &current.user.email),
        now,
    )
    .await?
    {
        Ok(attempt) => attempt,
        Err(paused) => {
            let (_, errors) = wrong(format!(
                "can't be checked right now: too many wrong codes. Try again in {}",
                paused.wait_text(now)
            ));
            return account::confirm_error(
                &s,
                ctx,
                &current,
                "magic_link",
                StatusCode::TOO_MANY_REQUESTS,
                None,
                errors,
            )
            .await;
        }
    };
    let outcome =
        login_link::check_code(&s.db, &s.config.auth.secret_key, target, &params.code).await?;
    let link = match outcome {
        // SECURITY: the code must have been sent to this person's address
        // (test: a_reauth_code_only_confirms_its_own_account)
        CodeCheck::Valid(link) if link.email == current.user.email => {
            auth_throttle::succeeded(&s.db, attempt).await?;
            link
        }
        CodeCheck::Wrong { left } => {
            auth_event::record(
                &s.db,
                Some(current.user.id),
                Kind::SignInFailed,
                &meta.meta,
                now,
            )
            .await?;
            let (status, errors) = wrong(format!("isn't right ({left} tries left)"));
            return account::confirm_error(&s, ctx, &current, "magic_link", status, None, errors)
                .await;
        }
        _ => {
            let (status, errors) = wrong("has expired; ask for a new one".to_owned());
            return account::confirm_error(&s, ctx, &current, "magic_link", status, None, errors)
                .await;
        }
    };
    let txn = db::begin(&s.db).await?;
    let claimed = login_link::claim(&txn, &link).await?;
    txn.commit().await?;
    if !claimed {
        let (status, errors) = wrong("has expired; ask for a new one".to_owned());
        return account::confirm_error(&s, ctx, &current, "magic_link", status, None, errors).await;
    }
    auth::grant_step_up(&s, &headers, &meta, &current).await
}
