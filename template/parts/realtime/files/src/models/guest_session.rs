//! A guest's signed-in browser: a random cookie token, stored only as its SHA-256 digest, so a
//! database leak leaks no sessions. Revalidated by every live connection
//! (`Channels::session_valid`); deleting the row ends the session and closes its sockets.
//!
//! APP: with accounts installed, delete this and use your sessions (see `guest.rs`).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::models::guest;

pub use session::{Column, Entity, Model};

pub mod session {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "guest_sessions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub guest_id: i64,
        pub token_digest: String,
        pub created_at: DateTimeUtc,
        pub last_seen_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "crate::models::guest::entity::Entity",
            from = "Column::GuestId",
            to = "crate::models::guest::entity::Column::Id",
            on_delete = "Cascade"
        )]
        Guest,
    }

    impl Related<crate::models::guest::entity::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Guest.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A session unused for this long has ended. APP: your idle timeout.
pub const IDLE: Duration = Duration::days(30);

/// 32 random bytes, URL-safe (43 characters).
pub fn generate_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// Lowercase hex SHA-256: what the database stores and is searched by.
pub fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Starts a session: the row, and the raw token for the cookie (never stored).
pub async fn start(
    db: &impl ConnectionTrait,
    guest_id: i64,
    now: DateTime<Utc>,
) -> Result<(Model, String), AppError> {
    let token = generate_token();
    let row = session::ActiveModel {
        id: NotSet,
        guest_id: Set(guest_id),
        token_digest: Set(digest(&token)),
        created_at: Set(now),
        last_seen_at: Set(now),
    }
    .insert(db)
    .await?;
    Ok((row, token))
}

/// The live session for a cookie token, with its guest. Bumps `last_seen_at` at most hourly.
pub async fn find_by_token(
    db: &impl ConnectionTrait,
    token: &str,
    now: DateTime<Utc>,
) -> Result<Option<(Model, guest::Model)>, AppError> {
    let found = Entity::find()
        .filter(Column::TokenDigest.eq(digest(token)))
        .filter(Column::LastSeenAt.gt(now - IDLE))
        .find_also_related(guest::Entity)
        .one(db)
        .await?;
    let Some((session, Some(guest))) = found else {
        return Ok(None);
    };
    if now - session.last_seen_at > Duration::hours(1) {
        let mut active: session::ActiveModel = session.clone().into();
        active.last_seen_at = Set(now);
        active.update(db).await?;
    }
    Ok(Some((session, guest)))
}

/// The live session `id`, with its guest (socket tokens carry a session id).
pub async fn find_live(
    db: &impl ConnectionTrait,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Option<(Model, guest::Model)>, AppError> {
    let found = Entity::find_by_id(id)
        .filter(Column::LastSeenAt.gt(now - IDLE))
        .find_also_related(guest::Entity)
        .one(db)
        .await?;
    Ok(match found {
        Some((session, Some(guest))) => Some((session, guest)),
        _ => None,
    })
}

/// Has session `id` neither been deleted nor gone idle?
pub async fn is_live(
    db: &impl ConnectionTrait,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    Ok(find_live(db, id, now).await?.is_some())
}

/// Sign out this browser.
pub async fn destroy(db: &impl ConnectionTrait, id: i64) -> Result<(), AppError> {
    Entity::delete_by_id(id).exec(db).await?;
    Ok(())
}

/// Sign out everywhere.
pub async fn destroy_all_for(db: &impl ConnectionTrait, guest_id: i64) -> Result<u64, AppError> {
    let res = Entity::delete_many()
        .filter(Column::GuestId.eq(guest_id))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_random_and_only_digests_are_stored() {
        let (a, b) = (generate_token(), generate_token());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert_eq!(digest(&a).len(), 64);
        assert_ne!(digest(&a), a);
    }
}
