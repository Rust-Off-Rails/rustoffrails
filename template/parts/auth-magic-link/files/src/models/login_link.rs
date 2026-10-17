//! Passwordless sign-in: an emailed link **and** a 6-digit code (for phones that open links in
//! another browser). Both are single-use, hashed and short-lived.
//!
//! GUIDE: .offrails/guides/auth.md#magic-link-variants-magic-link-magic-link-invite
//!
//! - The link's token is 256 random bits; only its SHA-256 is stored.
//! - The code is 6 digits; only its HMAC (keyed by `APP_SECRET_KEY`) is stored, and a link allows
//!   `MAX_CODE_ATTEMPTS` wrong codes before it's burned. Wrong codes also count per address,
//!   across every link, in `auth_throttles` (the controller), so asking for new links doesn't
//!   buy new guesses.
//! - Every link remembers the browser that asked for it (a random cookie, stored as a digest).
//!   The code is only accepted in that browser; by default the link is too (`same_browser`).
//! - Using a link or code deletes the row in the same transaction that starts the session: the
//!   DELETE is the claim, so two racing requests can't both sign in.
//! - Minted in the background task that sends the email, never in the request.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveValue::Set,
    QueryOrder,
    entity::prelude::*,
    sea_query::{Expr, ExprTrait},
};

use crate::{error::AppError, kernel::auth_token};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "login_links")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The address, not a user id: the same flow signs up a first-time visitor.
    pub email: String,
    pub purpose: Purpose,
    pub token_digest: String,
    pub code_digest: String,
    pub browser_digest: String,
    pub code_attempts: i32,
    pub expires_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum Purpose {
    /// Sign in (or sign up on first use).
    #[sea_orm(string_value = "sign_in")]
    SignIn,
    /// Confirm identity before a sensitive action (code only).
    #[sea_orm(string_value = "reauth")]
    Reauth,
}

// APP: link and code lifetime. 15 minutes is the framework default; shorter is fine.
pub const TTL: Duration = Duration::minutes(15);
// APP: wrong codes allowed per link before it's burned (5 → a 1-in-200,000 chance per link).
pub const MAX_CODE_ATTEMPTS: i32 = 5;
const CODE_DIGITS: u32 = 6;

/// A freshly minted link: the raw token and code, for the email only.
pub struct Minted {
    pub token: String,
    pub code: String,
}

/// Mints a link for `email`; earlier links for the address (and purpose) stop working.
pub async fn mint(
    db: &impl ConnectionTrait,
    key: &[u8],
    email: &str,
    purpose: Purpose,
    browser_digest: &str,
    now: DateTime<Utc>,
) -> Result<Minted, AppError> {
    Entity::delete_many()
        .filter(Column::Email.eq(email))
        .filter(Column::Purpose.eq(purpose))
        .exec(db)
        .await?;
    let token = auth_token::generate_token();
    let code = auth_token::generate_code(CODE_DIGITS);
    ActiveModel {
        email: Set(email.to_owned()),
        purpose: Set(purpose),
        token_digest: Set(auth_token::digest(&token)),
        code_digest: Set(code_digest(key, &code)),
        browser_digest: Set(browser_digest.to_owned()),
        code_attempts: Set(0),
        expires_at: Set(now + TTL),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(Minted { token, code })
}

fn code_digest(key: &[u8], code: &str) -> String {
    auth_token::keyed_digest(key, "login_code", code)
}

/// A usable sign-in link by its raw token, without consuming it (the confirm page).
pub async fn find_by_token(
    db: &impl ConnectionTrait,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Option<Model>, AppError> {
    if raw.len() > 128 {
        return Ok(None);
    }
    Ok(Entity::find()
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .filter(Column::Purpose.eq(Purpose::SignIn))
        .filter(Column::ExpiresAt.gt(now))
        .one(db)
        .await?)
}

/// Does this browser's cookie match the one that asked for the link?
// SECURITY: constant-time comparison of digests
// (test: the_code_signs_in_only_in_the_browser_that_asked)
pub fn same_browser(link: &Model, browser_cookie: Option<&str>) -> bool {
    browser_cookie.is_some_and(|raw| {
        auth_token::constant_time_eq(
            auth_token::digest(raw).as_bytes(),
            link.browser_digest.as_bytes(),
        )
    })
}

/// Claims the link: deletes it and reports whether this call won. Run inside the transaction
/// that starts the session.
// SECURITY: the DELETE is the claim; of two racing requests exactly one sees rows_affected == 1.
// INVARIANT: a link signs in once (test: a_link_works_once_even_when_used_twice_at_the_same_time)
pub async fn claim(db: &impl ConnectionTrait, link: &Model) -> Result<bool, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::Id.eq(link.id))
        .exec(db)
        .await?
        .rows_affected
        == 1)
}

/// The outcome of typing a code.
pub enum CodeCheck {
    /// Right code: the link (not yet claimed).
    Valid(Model),
    /// Wrong code; `left` tries remain on this link.
    Wrong { left: i32 },
    /// No usable link for this browser (none, expired, or burned by too many wrong codes).
    NoLink,
}

/// The newest usable link this browser asked for (by its cookie), without counting anything:
/// the code form checks the address's throttle with it before `check_code`.
pub async fn newest_for_browser(
    db: &impl ConnectionTrait,
    purpose: Purpose,
    browser_cookie: &str,
    now: DateTime<Utc>,
) -> Result<Option<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::BrowserDigest.eq(auth_token::digest(browser_cookie)))
        .filter(Column::Purpose.eq(purpose))
        .filter(Column::ExpiresAt.gt(now))
        .order_by_desc(Column::Id)
        .one(db)
        .await?)
}

/// Checks a typed code against `link` (from `newest_for_browser`).
// SECURITY: an attempt is counted (atomically) **before** comparing, so parallel guesses can't
// exceed MAX_CODE_ATTEMPTS; the comparison is constant-time on keyed digests.
// INVARIANT: a link allows at most 5 wrong codes (test: a_code_allows_five_wrong_guesses_then_burns_the_link)
pub async fn check_code(
    db: &impl ConnectionTrait,
    key: &[u8],
    link: Model,
    code: &str,
) -> Result<CodeCheck, AppError> {
    let counted = Entity::update_many()
        .col_expr(Column::CodeAttempts, Expr::col(Column::CodeAttempts).add(1))
        .filter(Column::Id.eq(link.id))
        .filter(Column::CodeAttempts.lt(MAX_CODE_ATTEMPTS))
        .exec(db)
        .await?;
    if counted.rows_affected != 1 {
        burn(db, link.id).await?;
        return Ok(CodeCheck::NoLink);
    }
    let typed = auth_token::digits_only(code);
    if typed.len() == CODE_DIGITS as usize
        && auth_token::constant_time_eq(
            code_digest(key, &typed).as_bytes(),
            link.code_digest.as_bytes(),
        )
    {
        return Ok(CodeCheck::Valid(link));
    }
    let left = MAX_CODE_ATTEMPTS - (link.code_attempts + 1);
    if left <= 0 {
        burn(db, link.id).await?;
        return Ok(CodeCheck::NoLink);
    }
    Ok(CodeCheck::Wrong { left })
}

async fn burn(db: &impl ConnectionTrait, id: i64) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(())
}

/// For `app task auth_cleanup`.
pub async fn delete_expired(
    db: &impl ConnectionTrait,
    now: DateTime<Utc>,
) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::ExpiresAt.lte(now))
        .exec(db)
        .await?
        .rows_affected)
}
