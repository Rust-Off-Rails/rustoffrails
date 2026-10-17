//! Progressive delays for guessing: wrong passwords, wrong two-factor codes, wrong emailed
//! codes. Database-backed, so the limit holds across processes and restarts.
//!
//! GUIDE: .offrails/guides/security.md#rate-limiting-kernelrate_limitrs
//!
//! The rule (`delay_after`): the first few failures are free; then each failure pauses that key
//! for twice as long as the last, up to a cap. While paused, attempts are refused **without
//! being counted**, so hammering never extends the pause, and the person is told exactly when
//! to come back. A success, or a password reset, clears the key.
//!
//! The key is an HMAC of e.g. `"login:ada@example.com"`, so unknown addresses are throttled
//! exactly like known ones (no enumeration) and the table holds no addresses.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveValue::Set, QuerySelect, entity::prelude::*, sea_query::Expr};

use crate::{error::AppError, kernel::db};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "auth_throttles")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// Attempts counted since the last success (or since the key went quiet for a day).
    pub failures: i32,
    pub blocked_until: Option<DateTimeUtc>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// How many failures are free before the first pause.
// APP: the free attempts. 5 lets a person fumble; a script gets 5 guesses, then waits.
pub const FREE_FAILURES: i32 = 5;
// APP: the first pause and the cap. 30 s, 1 m, 2 m, 4 m, 8 m, then 15 m per further failure.
const FIRST_PAUSE_SECS: i64 = 30;
const MAX_PAUSE_SECS: i64 = 15 * 60;
/// A key with no failure for this long starts from zero again.
const FORGET_AFTER: Duration = Duration::hours(24);

/// The pause that follows failure number `n` (1-based).
// INVARIANT: delays grow, then cap (test: throttle_delays_grow_then_cap)
pub fn delay_after(n: i32) -> Option<Duration> {
    if n < FREE_FAILURES {
        return None;
    }
    let doublings = (n - FREE_FAILURES).min(20) as u32;
    let secs = FIRST_PAUSE_SECS.saturating_mul(1i64 << doublings);
    Some(Duration::seconds(secs.min(MAX_PAUSE_SECS)))
}

/// An attempt that was allowed: it has already been counted as a failure; call `succeeded` to
/// forgive it.
#[derive(Debug)]
pub struct Attempt {
    pub key: String,
    /// The failure number this attempt would be.
    pub number: i32,
}

/// Refused: come back at `until`.
#[derive(Debug, Clone, Copy)]
pub struct Paused {
    pub until: DateTime<Utc>,
}

impl Paused {
    /// "30 seconds", "4 minutes": for the message the person sees.
    pub fn wait_text(&self, now: DateTime<Utc>) -> String {
        let secs = (self.until - now).num_seconds().max(1);
        if secs < 90 {
            format!("{secs} seconds")
        } else {
            format!("{} minutes", (secs + 59) / 60)
        }
    }
}

/// Reserves one attempt for `key`: refuses while paused, otherwise counts the attempt up front
/// (and starts the next pause if this is past the free ones), then lets the caller check the
/// secret.
// SECURITY: counted *before* the check, under a row lock (Postgres FOR UPDATE, SQLite BEGIN
// IMMEDIATE), so 100 parallel guesses can't all slip through before the first failure lands.
// INVARIANT: parallel guesses can't exceed the free attempts (test: parallel_guesses_are_counted_up_front)
// INVARIANT: refused attempts never extend the pause (test: refused_attempts_do_not_extend_the_pause)
pub async fn begin(
    conn: &DatabaseConnection,
    key: &str,
    now: DateTime<Utc>,
) -> Result<Result<Attempt, Paused>, AppError> {
    let txn = db::begin(conn).await?;
    Entity::insert(ActiveModel {
        key: Set(key.to_owned()),
        failures: Set(0),
        blocked_until: Set(None),
        updated_at: Set(now),
    })
    .on_conflict_do_nothing_on([Column::Key])
    .exec_without_returning(&txn)
    .await?;
    let row = Entity::find_by_id(key.to_owned())
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(until) = row.blocked_until
        && until > now
    {
        txn.commit().await?;
        return Ok(Err(Paused { until }));
    }
    let failures = if now - row.updated_at > FORGET_AFTER {
        0
    } else {
        row.failures
    };
    let number = failures + 1;
    Entity::update_many()
        .col_expr(Column::Failures, Expr::value(number))
        .col_expr(
            Column::BlockedUntil,
            Expr::value(delay_after(number).map(|d| now + d)),
        )
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Key.eq(key))
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(Ok(Attempt {
        key: key.to_owned(),
        number,
    }))
}

/// The secret was right: forget the failures.
pub async fn succeeded(db: &impl ConnectionTrait, attempt: Attempt) -> Result<(), AppError> {
    clear(db, &attempt.key).await
}

/// Forgets a key (a successful reset unlocks sign-in at once).
pub async fn clear(db: &impl ConnectionTrait, key: &str) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::Key.eq(key))
        .exec(db)
        .await?;
    Ok(())
}

/// Is the key paused right now? (Peek, no counting.)
pub async fn paused(
    db: &impl ConnectionTrait,
    key: &str,
    now: DateTime<Utc>,
) -> Result<Option<Paused>, AppError> {
    Ok(Entity::find_by_id(key.to_owned())
        .one(db)
        .await?
        .and_then(|r| r.blocked_until)
        .filter(|until| *until > now)
        .map(|until| Paused { until }))
}

/// For `app task auth_cleanup`: keys quiet for a day.
pub async fn delete_stale(db: &impl ConnectionTrait, now: DateTime<Utc>) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::UpdatedAt.lt(now - FORGET_AFTER))
        .exec(db)
        .await?
        .rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_delays_grow_then_cap() {
        assert_eq!(delay_after(1), None);
        assert_eq!(delay_after(FREE_FAILURES - 1), None);
        assert_eq!(delay_after(FREE_FAILURES), Some(Duration::seconds(30)));
        assert_eq!(delay_after(FREE_FAILURES + 1), Some(Duration::seconds(60)));
        assert_eq!(delay_after(FREE_FAILURES + 4), Some(Duration::seconds(480)));
        assert_eq!(delay_after(FREE_FAILURES + 5), Some(Duration::seconds(900)));
        assert_eq!(delay_after(10_000), Some(Duration::seconds(900)));
    }
}
