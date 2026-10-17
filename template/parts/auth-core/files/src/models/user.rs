//! Accounts. Only who the person is lives here; how they prove it lives in each sign-in method's
//! own table (`password_credentials`, `login_links`, `totp_credentials`), so methods combine
//! without touching this file.
//!
//! GUIDE: .offrails/guides/auth.md#tables

use std::str::FromStr;

use chrono::{DateTime, Utc};
use sea_orm::{ActiveValue::Set, entity::prelude::*, sea_query::Expr};

use crate::{error::AppError, models::Errors};

// ---------- Entity: mirrors the migration exactly ----------

/// No `Serialize`: a user row can't be sent to a client by accident.
// WHY: deriving Serialize invites leaking columns into JSON or props.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Lowercased and trimmed (`normalize_email`); unique.
    pub email: String,
    /// When the person proved they read this address. `None` until then.
    pub email_verified_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::session::Entity")]
    Sessions,
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sessions.def()
    }
}

/// Must stay empty: callbacks are banned (.offrails/guides/models.md).
impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn is_verified(&self) -> bool {
        self.email_verified_at.is_some()
    }
}

// ---------- Email ----------

pub const MAX_EMAIL_CHARS: usize = 254;

/// Trimmed and lowercased. Run on every address before comparing or storing.
// SECURITY: without one normalization, "Ada@Example.com" and "ada@example.com" become two
// accounts, two rate-limit keys and two throttles (test: normalizes_and_validates)
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// The normalized address, or an error on the `email` field.
pub fn validate_email(raw: &str) -> Result<String, Errors> {
    let email = normalize_email(raw);
    if email.is_empty() {
        return Err(Errors::single("email", "can't be blank"));
    }
    if email.chars().count() > MAX_EMAIL_CHARS
        || email.chars().any(|c| c.is_whitespace() || c.is_control())
        || lettre::Address::from_str(&email).is_err()
    {
        return Err(Errors::single("email", "is invalid"));
    }
    Ok(email)
}

// ---------- Queries ----------

pub async fn find_by_email(
    db: &impl ConnectionTrait,
    email: &str,
) -> Result<Option<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::Email.eq(normalize_email(email)))
        .one(db)
        .await?)
}

/// The signed-in person's own row. The actor *is* the record, so this is the scoped lookup.
pub async fn find_self(db: &impl ConnectionTrait, user_id: i64) -> Result<Model, AppError> {
    Entity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// Inserts the user unless the address already has one, then returns it. Never fails on a
/// duplicate, so two first sign-ins racing both succeed.
// WHY: a caught unique violation aborts a Postgres transaction; ON CONFLICT DO NOTHING doesn't.
// INVARIANT: works twice inside one Postgres transaction
// (test: find_or_create_twice_in_one_transaction_keeps_it_usable)
pub async fn find_or_create(
    db: &impl ConnectionTrait,
    email: &str,
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    let email = normalize_email(email);
    let new = ActiveModel {
        email: Set(email.clone()),
        email_verified_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    Entity::insert(new)
        .on_conflict_do_nothing_on([Column::Email])
        .exec_without_returning(db)
        .await?;
    find_by_email(db, &email).await?.ok_or(AppError::NotFound)
}

/// Marks the address verified (a link or code sent to it was used). Idempotent: `true` when this
/// call verified it, `false` when it already was. Inside a transaction the UPDATE also locks the
/// row until commit (Postgres), so whatever the caller does next to an account it just verified
/// can't interleave with a concurrent sign-up for the same address.
pub async fn mark_verified(
    db: &impl ConnectionTrait,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let updated = Entity::update_many()
        .col_expr(Column::EmailVerifiedAt, Expr::value(now))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(user_id))
        .filter(Column::EmailVerifiedAt.is_null())
        .exec(db)
        .await?;
    Ok(updated.rows_affected == 1)
}

/// Locks an **unverified** account's row for the rest of the transaction: `false` when it is
/// verified (or gone). A sign-up that replaces an unverified account's password takes this first,
/// so it either commits before the owner's verification (which then removes that password) or
/// waits for it and sees the account verified.
pub async fn lock_unverified(
    db: &impl ConnectionTrait,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let updated = Entity::update_many()
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(user_id))
        .filter(Column::EmailVerifiedAt.is_null())
        .exec(db)
        .await?;
    Ok(updated.rows_affected == 1)
}

/// Changes the address (after the new one was confirmed). `Err(Invalid)` when another account
/// took it meanwhile.
pub async fn change_email(
    db: &impl ConnectionTrait,
    user_id: i64,
    new_email: &str,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    if find_by_email(db, new_email).await?.is_some() {
        return Err(AppError::Invalid(Errors::single(
            "email",
            "is already used by another account",
        )));
    }
    Entity::update_many()
        .col_expr(Column::Email, Expr::value(normalize_email(new_email)))
        .col_expr(Column::EmailVerifiedAt, Expr::value(now))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_validates() {
        assert_eq!(
            validate_email("  Ada@Example.COM ").unwrap(),
            "ada@example.com"
        );
        assert!(validate_email("").is_err());
        assert!(validate_email("no-at-sign").is_err());
        assert!(validate_email("a b@example.com").is_err());
        assert!(validate_email("a\u{0}b@example.com").is_err());
        assert!(validate_email(&format!("{}@example.com", "a".repeat(250))).is_err());
    }
}
