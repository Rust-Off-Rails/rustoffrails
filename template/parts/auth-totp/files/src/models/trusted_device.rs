//! "Trust this device for 30 days": a random token in a cookie, its digest here. A trusted
//! browser skips the code step (never the password or link). Listed on the account page, revocable
//! one by one, and all revoked when the password changes, two-factor is turned off, or the
//! person says "this wasn't me".

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveValue::Set, QueryOrder, entity::prelude::*, sea_query::Expr};

use crate::{error::AppError, kernel::auth_token, models::user};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "trusted_devices")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub token_digest: String,
    pub ip: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    pub created_at: DateTimeUtc,
    pub last_used_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
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

// APP: how long "trust this device" lasts.
pub const TTL: Duration = Duration::days(30);

/// Trusts this browser for `user_id`; returns the raw token for the cookie.
pub async fn trust(
    db: &impl ConnectionTrait,
    user_id: i64,
    ip: Option<String>,
    user_agent: Option<String>,
    now: DateTime<Utc>,
) -> Result<String, AppError> {
    let raw = auth_token::generate_token();
    ActiveModel {
        user_id: Set(user_id),
        token_digest: Set(auth_token::digest(&raw)),
        ip: Set(ip),
        user_agent: Set(user_agent.map(|ua| ua.chars().take(400).collect())),
        created_at: Set(now),
        last_used_at: Set(now),
        expires_at: Set(now + TTL),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(raw)
}

/// Is this cookie a live trusted device **of this user**? Bumps `last_used_at`.
// SECURITY: scoped to the user signing in: another account's trusted-device cookie skips nothing
// (test: a_trusted_device_cookie_only_works_for_its_own_account)
pub async fn check(
    db: &impl ConnectionTrait,
    user_id: i64,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let res = Entity::update_many()
        .col_expr(Column::LastUsedAt, Expr::value(now))
        .filter(Column::UserId.eq(user_id))
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .filter(Column::ExpiresAt.gt(now))
        .exec(db)
        .await?;
    Ok(res.rows_affected == 1)
}

pub async fn all_for(
    db: &impl ConnectionTrait,
    user: &user::Model,
    now: DateTime<Utc>,
) -> Result<Vec<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user.id))
        .filter(Column::ExpiresAt.gt(now))
        .order_by_desc(Column::LastUsedAt)
        .all(db)
        .await?)
}

/// Forgets one of the person's trusted devices; another person's id is `NotFound`.
pub async fn revoke_for(
    db: &impl ConnectionTrait,
    user: &user::Model,
    id: i64,
) -> Result<(), AppError> {
    let res = Entity::delete_many()
        .filter(Column::Id.eq(id))
        .filter(Column::UserId.eq(user.id))
        .exec(db)
        .await?;
    if res.rows_affected == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
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
