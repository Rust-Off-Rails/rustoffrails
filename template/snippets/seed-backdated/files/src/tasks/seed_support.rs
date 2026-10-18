//! Seeds with a history (the seed-backdated snippet). Create each record the normal way, through
//! its model's `create` (validation runs, the row is real), then move its timestamps into the past
//! with `backdate`: `create` stamps `now`, so without it every seeded post is "just now" and every
//! list sorted by date shows them in insertion order.
//! GUIDE: .offrails/template/snippets/seed-backdated/README.md

use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};

/// `days` days before now (a seed's "published a month ago").
pub fn days_ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

/// Sets `created_at` and `updated_at` of the row `id` of `E`'s table to `at`. Every generated
/// table has both columns. Seeds and tests only: in the app, a timestamp is what happened.
pub async fn backdate<E: EntityTrait>(
    db: &impl ConnectionTrait,
    id: i64,
    at: DateTime<Utc>,
) -> Result<(), DbErr> {
    let table = E::default().table_name();
    let stmt = Query::update()
        .table(Alias::new(table))
        .value(Alias::new("created_at"), at)
        .value(Alias::new("updated_at"), at)
        .and_where(Expr::col(Alias::new("id")).eq(id))
        .to_owned();
    let done = db.execute(&stmt).await?;
    if done.rows_affected() == 1 {
        Ok(())
    } else {
        Err(DbErr::RecordNotFound(format!("{table} {id}")))
    }
}
