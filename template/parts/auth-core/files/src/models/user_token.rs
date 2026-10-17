//! Single-use emailed tokens for an existing account: email verification, password reset,
//! email change and "this wasn't me". One table, one recipe, like phx.gen.auth's `users_tokens`.
//!
//! GUIDE: .offrails/guides/auth.md#password-reset-passwordsrs-new-create-edit-update
//!
//! - Only the SHA-256 of the token is stored.
//! - Each token has a purpose and a lifetime; it's valid only for that purpose.
//! - `sent_to` is the address it went to. Verification and reset tokens are valid only while
//!   the account still has that address, so changing the email kills older links.
//! - Consuming deletes the row inside the caller's transaction; the delete is the claim.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveValue::Set, entity::prelude::*};

use crate::{error::AppError, kernel::auth_token};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub purpose: Purpose,
    pub token_digest: String,
    pub sent_to: String,
    /// SHA-256 of a cookie set in the browser that asked for the token, when the flow binds the
    /// token to that browser (sign-up verification). `None`: any browser.
    pub browser_digest: Option<String>,
    pub expires_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(32))")]
pub enum Purpose {
    #[sea_orm(string_value = "email_verification")]
    EmailVerification,
    #[sea_orm(string_value = "password_reset")]
    PasswordReset,
    /// `sent_to` is the **new** address.
    #[sea_orm(string_value = "email_change")]
    EmailChange,
    /// The link in security notices: signs the account out everywhere.
    #[sea_orm(string_value = "not_me")]
    NotMe,
}

impl Purpose {
    /// How long a link of this kind works.
    // APP: lifetimes. Keep resets short (they grant the account); verification can be longer.
    pub fn ttl(self) -> Duration {
        match self {
            Purpose::EmailVerification => Duration::hours(24),
            Purpose::PasswordReset => Duration::minutes(30),
            Purpose::EmailChange => Duration::hours(1),
            Purpose::NotMe => Duration::days(7),
        }
    }

    /// Only one live token of this kind per account: issuing a new one revokes the older ones.
    fn single_live(self) -> bool {
        !matches!(self, Purpose::NotMe)
    }

    /// Valid only while the account's email still equals `sent_to`.
    fn bound_to_current_email(self) -> bool {
        matches!(self, Purpose::EmailVerification | Purpose::PasswordReset)
    }
}

/// Mints a token and returns the raw value for the email. Call it **inside** the background
/// task that sends the email, never in the request, so the token is never stored anywhere else.
// SECURITY: digest only; older tokens of the same purpose stop working
// (test: only_token_digests_are_stored)
pub async fn issue(
    db: &impl ConnectionTrait,
    user_id: i64,
    purpose: Purpose,
    sent_to: &str,
    browser_digest: Option<String>,
    now: DateTime<Utc>,
) -> Result<String, AppError> {
    if purpose.single_live() {
        Entity::delete_many()
            .filter(Column::UserId.eq(user_id))
            .filter(Column::Purpose.eq(purpose))
            .exec(db)
            .await?;
    }
    let raw = auth_token::generate_token();
    ActiveModel {
        user_id: Set(user_id),
        purpose: Set(purpose),
        token_digest: Set(auth_token::digest(&raw)),
        sent_to: Set(sent_to.to_owned()),
        browser_digest: Set(browser_digest),
        expires_at: Set(now + purpose.ttl()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(raw)
}

/// The token's row with its user, if it is valid for `purpose` right now. Doesn't consume it
/// (confirm pages call this on GET, so link scanners can't burn the token).
pub async fn find_valid(
    db: &impl ConnectionTrait,
    purpose: Purpose,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Option<(Model, super::user::Model)>, AppError> {
    if raw.len() > 128 {
        return Ok(None);
    }
    let found = Entity::find()
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .filter(Column::Purpose.eq(purpose))
        .filter(Column::ExpiresAt.gt(now))
        .find_also_related(super::user::Entity)
        .one(db)
        .await?;
    Ok(found.and_then(|(t, u)| {
        let u = u?;
        // SECURITY: a reset or verification link sent to an old address dies with the change
        // (test: a_reset_link_sent_to_an_old_address_dies_with_the_change)
        (!purpose.bound_to_current_email() || u.email == t.sent_to).then_some((t, u))
    }))
}

/// Consumes the token: deletes it and returns it with its user. `None` when it is invalid or
/// a concurrent request consumed it first. Run inside the transaction that applies the effect.
// SECURITY: the DELETE is the claim; exactly one of two racing requests gets rows_affected == 1.
// INVARIANT: a token works once (test: a_reset_link_works_once)
pub async fn consume(
    db: &impl ConnectionTrait,
    purpose: Purpose,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Option<(Model, super::user::Model)>, AppError> {
    let Some((token, user)) = find_valid(db, purpose, raw, now).await? else {
        return Ok(None);
    };
    let claimed = Entity::delete_many()
        .filter(Column::Id.eq(token.id))
        .exec(db)
        .await?;
    Ok((claimed.rows_affected == 1).then_some((token, user)))
}

/// Revokes every token of a purpose for the account (password changed: older reset links die).
pub async fn revoke_all(
    db: &impl ConnectionTrait,
    user_id: i64,
    purpose: Purpose,
) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .filter(Column::Purpose.eq(purpose))
        .exec(db)
        .await?;
    Ok(())
}

/// Revokes every emailed token of the account, whatever its purpose (an unverified account
/// claimed by its address's owner: `auth::claim_unverified_account`).
pub async fn revoke_every_purpose(db: &impl ConnectionTrait, user_id: i64) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
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
