//! Database-backed sessions (Rails 8 style). The cookie holds a random token; only its SHA-256 is
//! stored, so a leaked table logs nobody in.
//!
//! GUIDE: .offrails/guides/auth.md#sessions

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveValue::Set, Condition, QueryOrder, entity::prelude::*, sea_query::Expr};

use crate::{
    error::AppError,
    kernel::{auth_token, session as auth_cookie},
    models::user,
};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub token_digest: String,
    pub ip: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    /// How this session began: "password", "magic_link", "password+totp", …
    pub sign_in_method: String,
    /// The last time the person proved who they are in this session (sign-in or re-auth).
    /// Sensitive actions require it to be recent (`RequireRecentAuth`).
    pub authenticated_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub last_seen_at: DateTimeUtc,
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

/// `last_seen_at` is refreshed at most this often: enough for "active 5 minutes ago" in the
/// sessions list, without a write on every request.
const TOUCH_EVERY: Duration = Duration::minutes(5);

/// Starts a session and returns it with the raw token (the only time the token exists in the
/// clear: it goes straight into the cookie).
pub async fn start(
    db: &impl ConnectionTrait,
    user_id: i64,
    sign_in_method: &str,
    ip: Option<String>,
    user_agent: Option<String>,
    now: DateTime<Utc>,
) -> Result<(Model, String), AppError> {
    let raw = auth_token::generate_token();
    let session = ActiveModel {
        user_id: Set(user_id),
        token_digest: Set(auth_token::digest(&raw)),
        ip: Set(ip),
        user_agent: Set(user_agent.map(|ua| ua.chars().take(400).collect())),
        sign_in_method: Set(sign_in_method.to_owned()),
        authenticated_at: Set(now),
        created_at: Set(now),
        last_seen_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok((session, raw))
}

/// One query: the live session for a raw cookie token, with its user.
// SECURITY: looked up by digest (never the raw token); idle and absolute expiry are enforced in
// the query, so an old stolen cookie stops working on its own.
// INVARIANT: an idle or too-old session logs nobody in (test: expired_sessions_log_nobody_in)
pub async fn find_by_token(
    db: &impl ConnectionTrait,
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Option<(Model, user::Model)>, AppError> {
    let found = Entity::find()
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .filter(Column::LastSeenAt.gt(now - Duration::days(auth_cookie::IDLE_DAYS)))
        .filter(Column::CreatedAt.gt(now - Duration::days(auth_cookie::ABSOLUTE_DAYS)))
        .find_also_related(user::Entity)
        .one(db)
        .await?;
    Ok(found.and_then(|(s, u)| u.map(|u| (s, u))))
}

/// Bumps `last_seen_at`, at most every few minutes.
pub async fn touch(
    db: &impl ConnectionTrait,
    session: &Model,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    if now - session.last_seen_at < TOUCH_EVERY {
        return Ok(());
    }
    Entity::update_many()
        .col_expr(Column::LastSeenAt, Expr::value(now))
        .filter(Column::Id.eq(session.id))
        .exec(db)
        .await?;
    Ok(())
}

/// Gives the session a new token (and re-auth time) and returns it. Called after a privilege
/// change: re-authentication, enabling two-factor.
// SECURITY: rotating on privilege change means a token captured before (a shoulder-surfed
// cookie, a fixation attempt) doesn't inherit the new privilege.
// INVARIANT: re-auth rotates the session token (test: step_up_rotates_the_session_token)
pub async fn rotate(
    db: &impl ConnectionTrait,
    session_id: i64,
    now: DateTime<Utc>,
) -> Result<String, AppError> {
    let raw = auth_token::generate_token();
    Entity::update_many()
        .col_expr(Column::TokenDigest, Expr::value(auth_token::digest(&raw)))
        .col_expr(Column::AuthenticatedAt, Expr::value(now))
        .col_expr(Column::LastSeenAt, Expr::value(now))
        .filter(Column::Id.eq(session_id))
        .exec(db)
        .await?;
    Ok(raw)
}

/// The person's sessions, newest activity first.
pub async fn all_for(
    db: &impl ConnectionTrait,
    user: &user::Model,
) -> Result<Vec<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user.id))
        .order_by_desc(Column::LastSeenAt)
        .order_by_desc(Column::Id)
        .all(db)
        .await?)
}

/// Ends one of the person's own sessions. Another person's id is `NotFound`.
// SECURITY: scoped by user_id in the query, so a guessed id can't sign someone else out.
// INVARIANT: revoking another user's session is a 404 (test: revoking_another_users_session_is_404)
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

/// Ends every session of the person except `keep`.
pub async fn revoke_others(
    db: &impl ConnectionTrait,
    user: &user::Model,
    keep: i64,
) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::UserId.eq(user.id))
        .filter(Column::Id.ne(keep))
        .exec(db)
        .await?
        .rows_affected)
}

/// Signs the person out everywhere (password change or reset, "this wasn't me").
pub async fn revoke_all(db: &impl ConnectionTrait, user_id: i64) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?
        .rows_affected)
}

/// Ends the session a raw cookie belongs to (sign-out, or sign-in over an old session).
pub async fn destroy_by_token(db: &impl ConnectionTrait, raw: &str) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::TokenDigest.eq(auth_token::digest(raw)))
        .exec(db)
        .await?;
    Ok(())
}

/// For `app task auth_cleanup`: sessions past either expiry.
pub async fn delete_expired(
    db: &impl ConnectionTrait,
    now: DateTime<Utc>,
) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(
            Condition::any()
                .add(Column::LastSeenAt.lte(now - Duration::days(auth_cookie::IDLE_DAYS)))
                .add(Column::CreatedAt.lte(now - Duration::days(auth_cookie::ABSOLUTE_DAYS))),
        )
        .exec(db)
        .await?
        .rows_affected)
}
