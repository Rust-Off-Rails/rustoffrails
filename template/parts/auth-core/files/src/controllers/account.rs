//! The account security page: sessions (revoke one or all others), recent security activity,
//! the email address, and a section per installed sample (password, two-factor). Plus
//! re-authentication ("confirm it's you") and email changes.
//!
//! GUIDE: .offrails/guides/auth.md
//! GUIDE: .offrails/guides/security.md#non-negotiables (9: session revocation)

use std::time::Duration as StdDuration;

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    controllers::{
        application::ClientIp,
        auth::{self, CurrentSession, RequestMeta, RequireRecentAuth},
    },
    error::AppError,
    kernel::{db, flash::Flash},
    mailers::auth_mailer::{self, Notice},
    models::{
        Errors,
        auth_event::{self, Kind},
        session, user, user_token,
    },
    routes::paths,
    state::AppState,
    views::{
        self, Ctx,
        auth::{
            AccountConfirm, AccountShow, CtxExt, EmailConfirm, EmailEdit, EventRow, InvalidLink,
            SessionRow,
        },
    },
};

// APP: how many events the account page lists.
const EVENTS_SHOWN: u64 = 12;

/// `GET /account`
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
) -> Result<Response, AppError> {
    let CurrentSession { session: me, user } = current;
    let now = Utc::now();
    let sessions = session::all_for(&s.db, &user)
        .await?
        .iter()
        .map(|x| SessionRow::new(x, me.id, now))
        .collect();
    let events = auth_event::recent_for(&s.db, &user, EVENTS_SHOWN)
        .await?
        .iter()
        .map(|e| EventRow::new(e, now))
        .collect();
    let recently_confirmed = now - me.authenticated_at <= auth::REAUTH_WINDOW;
    // samples: account load
    let ctx = ctx.titled("Account security").noindex();
    Ok(views::render(AccountShow {
        ctx,
        user,
        sessions,
        events,
        recently_confirmed,
        // samples: account init
    })?
    .into_response())
}

/// `POST /account/sessions/{id}/delete`: signs out one of the person's other devices.
pub async fn revoke(
    State(s): State<AppState>,
    current: CurrentSession,
    meta: RequestMeta,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if id == current.session.id {
        // Signing out this device is the sign-out button's job (it also clears the cookie).
        return Ok((
            Flash::alert("Use \"Sign out\" to end this session."),
            Redirect::to(&paths::account()),
        )
            .into_response());
    }
    // SECURITY: scoped to the signed-in person; another person's session id is a 404
    // (test: revoking_another_users_session_is_404)
    session::revoke_for(&s.db, &current.user, id).await?;
    auth_event::record(
        &s.db,
        Some(current.user.id),
        Kind::SessionRevoked,
        &meta.meta,
        Utc::now(),
    )
    .await?;
    auth::sessions_ended(&s, current.user.id);
    Ok((
        Flash::notice("That device was signed out."),
        Redirect::to(&paths::account()),
    )
        .into_response())
}

/// `POST /account/sessions/delete-others`: "Sign out everywhere else".
pub async fn revoke_others(
    State(s): State<AppState>,
    current: CurrentSession,
    meta: RequestMeta,
) -> Result<Response, AppError> {
    let n = session::revoke_others(&s.db, &current.user, current.session.id).await?;
    auth_event::record(
        &s.db,
        Some(current.user.id),
        Kind::OtherSessionsRevoked,
        &meta.meta,
        Utc::now(),
    )
    .await?;
    auth::sessions_ended(&s, current.user.id);
    let message = match n {
        0 => "There were no other devices to sign out.".to_owned(),
        1 => "Signed out 1 other device.".to_owned(),
        n => format!("Signed out {n} other devices."),
    };
    Ok((Flash::notice(message), Redirect::to(&paths::account())).into_response())
}

/// `GET /account/confirm`: "Confirm it's you", with a form per installed method.
pub async fn confirm(
    State(s): State<AppState>,
    ctx: Ctx,
    current: CurrentSession,
) -> Result<Response, AppError> {
    let page = confirm_page(&s, ctx, &current, "", None, Errors::default()).await?;
    Ok(views::render(page)?.into_response())
}

/// Builds the confirm page; the method samples call it to re-render with their errors.
pub async fn confirm_page(
    s: &AppState,
    ctx: Ctx,
    current: &CurrentSession,
    form: &'static str,
    alert: Option<String>,
    errors: Errors,
) -> Result<AccountConfirm, AppError> {
    let user = current.user.clone();
    let second_factor = auth::step_up_needs_second_factor(s, &user).await?;
    // samples: confirm load
    Ok(AccountConfirm {
        ctx: ctx.titled("Confirm it's you").noindex(),
        user,
        alert,
        errors,
        form,
        second_factor,
        // samples: confirm init
    })
}

/// Renders the confirm page with an error, with `status` (422 wrong secret, 429 paused).
pub async fn confirm_error(
    s: &AppState,
    ctx: Ctx,
    current: &CurrentSession,
    form: &'static str,
    status: StatusCode,
    alert: Option<String>,
    errors: Errors,
) -> Result<Response, AppError> {
    let page = confirm_page(s, ctx, current, form, alert, errors).await?;
    Ok((status, views::render(page)?).into_response())
}

// ---------------------------------------------------------------------------------------------
// Email change: re-auth, then a link to the new address; the old address is told.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct EmailParams {
    #[serde(default)]
    pub email: String,
}

// APP: how many email-change requests one account may send per hour.
const EMAIL_CHANGE_LIMIT: u32 = 5;
const EMAIL_CHANGE_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);

/// `GET /account/email/edit`
pub async fn edit_email(ctx: Ctx, auth: RequireRecentAuth) -> Result<Response, AppError> {
    let ctx = ctx.titled("Change email").noindex();
    Ok(views::render(EmailEdit {
        ctx,
        email: String::new(),
        user: auth.user,
        errors: Errors::default(),
    })?
    .into_response())
}

/// `POST /account/email`: emails a confirmation link to the new address. Nothing changes until
/// it's clicked.
// SECURITY: requires recent re-auth, and the new address must prove it reads its mail, or a
// typo (or an attacker with a session) could move the account to an address they control
// (test: sensitive_actions_require_recent_auth, changing_the_email_needs_the_new_address_to_confirm_and_warns_the_old_one)
pub async fn update_email(
    State(s): State<AppState>,
    ctx: Ctx,
    auth: RequireRecentAuth,
    meta: RequestMeta,
    ip: ClientIp,
    Form(params): Form<EmailParams>,
) -> Result<Response, AppError> {
    let invalid = |ctx: Ctx,
                   user: user::Model,
                   email: String,
                   errors: Errors|
     -> Result<Response, AppError> {
        Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            views::render(EmailEdit {
                ctx: ctx.titled("Change email").noindex(),
                user,
                email,
                errors,
            })?,
        )
            .into_response())
    };
    let new_email = match user::validate_email(&params.email) {
        Ok(e) => e,
        Err(errors) => return invalid(ctx, auth.user, params.email, errors),
    };
    if new_email == auth.user.email {
        return invalid(
            ctx,
            auth.user,
            params.email,
            Errors::single("email", "is already your address"),
        );
    }
    let key = format!("account#email:{}", auth.user.id);
    if s.rate_limit
        .check(&key, EMAIL_CHANGE_LIMIT, EMAIL_CHANGE_WINDOW)
        .await
        .is_err()
        || s.rate_limit
            .check(
                &ip.key("account#email"),
                EMAIL_CHANGE_LIMIT * 4,
                EMAIL_CHANGE_WINDOW,
            )
            .await
            .is_err()
    {
        return Err(AppError::TooManyRequests);
    }
    // WHY: an address taken by another account gets the same answer (and no mail), so this form
    // can't be used to find out who has an account.
    if user::find_by_email(&s.db, &new_email).await?.is_none() {
        let state = s.clone();
        let (user_id, to) = (auth.user.id, new_email.clone());
        s.background.spawn("email change confirmation", async move {
            let token = user_token::issue(
                &state.db,
                user_id,
                user_token::Purpose::EmailChange,
                &to,
                None,
                Utc::now(),
            )
            .await?;
            let email = auth_mailer::email_change(&state.config, &to, &token)?;
            state.mailer.deliver(email).await?;
            Ok(())
        });
    }
    auth_event::record(
        &s.db,
        Some(auth.user.id),
        Kind::EmailChangeRequested,
        &meta.meta,
        Utc::now(),
    )
    .await?;
    Ok((
        Flash::notice(format!(
            "We sent a confirmation link to {new_email}. Your address changes when you click it."
        )),
        Redirect::to(&paths::account()),
    )
        .into_response())
}

/// `GET /account/email/confirm/{token}`: a confirm button; GET never changes anything, so mail
/// scanners that open links can't use the token up.
// SECURITY: GET is safe (CSRF and link prefetchers); the POST does the change
// (test: changing_the_email_needs_the_new_address_to_confirm_and_warns_the_old_one)
pub async fn show_email_change(
    State(s): State<AppState>,
    ctx: Ctx,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ctx = ctx.at(&paths::account()).noindex();
    match user_token::find_valid(&s.db, user_token::Purpose::EmailChange, &token, Utc::now())
        .await?
    {
        Some((t, _)) => Ok(token_page(
            views::render(EmailConfirm {
                ctx: ctx.titled("Confirm your new email"),
                new_email: t.sent_to,
                action: paths::email_change(&token),
            })?
            .into_response(),
        )),
        None => invalid_link(
            ctx,
            "This confirmation link doesn't work",
            "Email confirmation links work once and expire after an hour. Ask for a new one from your account page.",
            "Go to your account",
            &paths::account(),
        ),
    }
}

/// `POST /account/email/confirm/{token}`: applies the change, verifies the new address and warns
/// the old one (with a "this wasn't me" link).
pub async fn confirm_email_change(
    State(s): State<AppState>,
    ctx: Ctx,
    meta: RequestMeta,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let txn = db::begin(&s.db).await?;
    let Some((t, u)) =
        user_token::consume(&txn, user_token::Purpose::EmailChange, &token, now).await?
    else {
        txn.rollback().await?;
        return invalid_link(
            ctx.at(&paths::account()).noindex(),
            "This confirmation link doesn't work",
            "Email confirmation links work once and expire after an hour. Ask for a new one from your account page.",
            "Go to your account",
            &paths::account(),
        );
    };
    if let Err(e) = user::change_email(&txn, u.id, &t.sent_to, now).await {
        txn.rollback().await?;
        return match e {
            AppError::Invalid(_) => Ok((
                Flash::alert("That address is already used by another account."),
                Redirect::to(&paths::account()),
            )
                .into_response()),
            other => Err(other),
        };
    }
    // Reset links and pending verifications sent to the old address die with it.
    user_token::revoke_all(&txn, u.id, user_token::Purpose::PasswordReset).await?;
    user_token::revoke_all(&txn, u.id, user_token::Purpose::EmailVerification).await?;
    auth_event::record(&txn, Some(u.id), Kind::EmailChanged, &meta.meta, now).await?;
    txn.commit().await?;
    auth::notify(
        &s,
        u.id,
        Notice::EmailChanged {
            old_email: u.email.clone(),
            new_email: t.sent_to.clone(),
        },
    );
    Ok((
        Flash::notice(format!("Your email address is now {}.", t.sent_to)),
        Redirect::to(&paths::account()),
    )
        .into_response())
}

/// The "invalid or expired link" page, 404, uncacheable and referrer-free.
pub fn invalid_link(
    ctx: Ctx,
    heading: &str,
    message: &str,
    link_label: &str,
    link_href: &str,
) -> Result<Response, AppError> {
    Ok(token_page(
        (
            StatusCode::NOT_FOUND,
            views::render(InvalidLink {
                ctx: ctx.titled(heading),
                heading: heading.to_owned(),
                message: message.to_owned(),
                link_label: link_label.to_owned(),
                link_href: link_href.to_owned(),
            })?,
        )
            .into_response(),
    ))
}

/// A page whose URL carries a token: never cached, never sent as a Referer.
// SECURITY: without these, the token leaks through shared caches, history snapshots and the
// Referer header of any link on the page.
// INVARIANT: token pages are no-store + no-referrer (test: token_pages_are_not_cached_and_send_no_referrer)
pub fn token_page(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
