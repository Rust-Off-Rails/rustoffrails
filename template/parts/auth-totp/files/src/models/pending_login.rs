//! The step between the first factor (password, link, reset) and the code: "half signed in".
//! A random token in a short-lived cookie, its digest here. It grants nothing but the right to
//! type a code, a limited number of times, for a few minutes.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveValue::Set,
    entity::prelude::*,
    sea_query::{Expr, ExprTrait},
};

use crate::{error::AppError, kernel::auth_token};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "pending_logins")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub token_digest: String,
    /// How the first factor was proved ("password", "magic_link", …).
    pub method: String,
    pub attempts: i32,
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

// APP: time allowed to type the code, and wrong codes allowed before starting over.
pub const TTL: Duration = Duration::minutes(10);
pub const MAX_ATTEMPTS: i32 = 5;

pub async fn start(
    db: &impl ConnectionTrait,
    user_id: i64,
    method: &str,
    now: DateTime<Utc>,
) -> Result<String, AppError> {
    let raw = auth_token::generate_token();
    ActiveModel {
        user_id: Set(user_id),
        token_digest: Set(auth_token::digest(&raw)),
        method: Set(method.to_owned()),
        attempts: Set(0),
        expires_at: Set(now + TTL),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(raw)
}

pub async fn find(
    db: &impl ConnectionTrait,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Option<Model>, AppError> {
    if raw.len() > 128 {
        return Ok(None);
    }
    Ok(Entity::find()
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .filter(Column::ExpiresAt.gt(now))
        .filter(Column::Attempts.lt(MAX_ATTEMPTS))
        .one(db)
        .await?)
}

/// Counts one attempt before the code is checked; `false` when none are left.
// SECURITY: counted up front and atomically, so parallel guesses can't exceed MAX_ATTEMPTS
// (test: a_pending_sign_in_allows_five_wrong_codes_then_starts_over)
pub async fn count_attempt(db: &impl ConnectionTrait, id: i64) -> Result<bool, AppError> {
    let res = Entity::update_many()
        .col_expr(Column::Attempts, Expr::col(Column::Attempts).add(1))
        .filter(Column::Id.eq(id))
        .filter(Column::Attempts.lt(MAX_ATTEMPTS))
        .exec(db)
        .await?;
    Ok(res.rows_affected == 1)
}

/// Ends it (success, or out of attempts). `true` if this call removed it (the claim).
pub async fn finish(db: &impl ConnectionTrait, id: i64) -> Result<bool, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::Id.eq(id))
        .exec(db)
        .await?
        .rows_affected
        == 1)
}

pub async fn revoke_all(db: &impl ConnectionTrait, user_id: i64) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

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
