//! `Rails.cache`: moka (L1, per process) in front of the `cache_entries` table (L2, shared).
//! Don't cache until it's measured or a spec requires it.

use std::{future::Future, sync::Arc, time::Duration};

use chrono::Utc;
use sea_orm::{
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter,
    sea_query::{LikeExpr, OnConflict},
};
use serde::{Serialize, de::DeserializeOwned};

pub mod entry {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "cache_entries")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        #[sea_orm(column_type = "Text")]
        pub value: String,
        pub expires_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

const L1_MAX_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Cache {
    /// Value plus its own expiry: the cache-wide TTL is only an upper bound.
    l1: moka::future::Cache<String, (Arc<String>, chrono::DateTime<Utc>)>,
    db: DatabaseConnection,
}

impl Cache {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            l1: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(L1_MAX_TTL)
                .support_invalidation_closures()
                .build(),
            db,
        }
    }

    pub async fn fetch<T, F, Fut, E>(&self, key: &str, ttl: Duration, compute: F) -> Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<DbErr>,
    {
        if let Some(hit) = self.read::<T>(key).await? {
            return Ok(hit);
        }
        let value = compute().await?;
        self.write(key, &value, ttl).await?;
        Ok(value)
    }

    pub async fn read<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, DbErr> {
        if let Some((json, expires_at)) = self.l1.get(key).await
            && expires_at > Utc::now()
        {
            // A shape change counts as a miss.
            if let Ok(v) = serde_json::from_str(&json) {
                return Ok(Some(v));
            }
        }
        let row = entry::Entity::find_by_id(key.to_owned())
            .one(&self.db)
            .await?;
        let Some(row) = row else { return Ok(None) };
        if row.expires_at <= Utc::now() {
            return Ok(None);
        }
        match serde_json::from_str(&row.value) {
            Ok(v) => {
                self.l1
                    .insert(key.to_owned(), (Arc::new(row.value), row.expires_at))
                    .await;
                Ok(Some(v))
            }
            Err(_) => Ok(None),
        }
    }

    pub async fn write<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), DbErr> {
        let json = serde_json::to_string(value).map_err(|e| DbErr::Custom(e.to_string()))?;
        let expires_at = Utc::now()
            + chrono::Duration::from_std(ttl).map_err(|e| DbErr::Custom(e.to_string()))?;
        entry::Entity::insert(entry::ActiveModel {
            key: Set(key.to_owned()),
            value: Set(json.clone()),
            expires_at: Set(expires_at),
        })
        .on_conflict(
            OnConflict::column(entry::Column::Key)
                .update_columns([entry::Column::Value, entry::Column::ExpiresAt])
                .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        self.l1
            .insert(key.to_owned(), (Arc::new(json), expires_at))
            .await;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<(), DbErr> {
        entry::Entity::delete_by_id(key.to_owned())
            .exec(&self.db)
            .await?;
        self.l1.invalidate(key).await;
        Ok(())
    }

    pub async fn delete_prefix(&self, prefix: &str) -> Result<(), DbErr> {
        // WHY: one `DELETE … WHERE key LIKE 'prefix%' ESCAPE '\\'` instead of loading every row
        // into Rust to filter (the table can hold 10k+ entries). `%`, `_` and `\` in the prefix
        // are escaped so they match literally on both SQLite and Postgres.
        entry::Entity::delete_many()
            .filter(entry::Column::Key.like(LikeExpr::new(like_prefix(prefix)).escape('\\')))
            .exec(&self.db)
            .await?;
        let prefix = prefix.to_owned();
        let _ = self
            .l1
            .invalidate_entries_if(move |k, _| k.starts_with(&prefix));
        Ok(())
    }

    /// Deletes expired L2 rows. Called by `CleanupJob`.
    pub async fn delete_expired(db: &DatabaseConnection) -> Result<u64, DbErr> {
        let res = entry::Entity::delete_many()
            .filter(entry::Column::ExpiresAt.lt(Utc::now()))
            .exec(db)
            .await?;
        Ok(res.rows_affected)
    }
}

/// `prefix` as a LIKE pattern matching keys that start with it: `%`, `_` and `\` escaped with
/// `\`, then a trailing `%`.
fn like_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_escaped_for_like() {
        assert_eq!(like_prefix("book:"), "book:%");
        assert_eq!(like_prefix("100%_x\\"), "100\\%\\_x\\\\%");
    }
}
