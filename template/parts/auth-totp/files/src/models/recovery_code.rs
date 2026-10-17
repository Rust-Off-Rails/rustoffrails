//! Ten single-use recovery codes per account, for when the phone is lost. Only their HMAC is
//! stored; each works once; generating new ones voids the old set.

use chrono::{DateTime, Utc};
use sea_orm::{ActiveValue::Set, PaginatorTrait, entity::prelude::*, sea_query::Expr};

use crate::{error::AppError, kernel::auth_token};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "recovery_codes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub code_digest: String,
    pub used_at: Option<DateTimeUtc>,
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

// APP: how many codes a set has.
pub const COUNT: u64 = 10;
/// Unambiguous characters only (no 0/O, 1/I/L), so a printed code is easy to type.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// `XXXXX-XXXXX`: 10 characters from a 31-letter alphabet, about 49 bits each.
fn generate_one() -> String {
    let mut out = String::with_capacity(11);
    for i in 0..10 {
        if i == 5 {
            out.push('-');
        }
        let idx: usize = rand::random_range(0..ALPHABET.len());
        out.push(ALPHABET[idx] as char);
    }
    out
}

/// What a person types, normalized: uppercase, no spaces or dashes.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn digest(key: &[u8], code: &str) -> String {
    auth_token::keyed_digest(key, "recovery_code", &normalize(code))
}

/// Replaces the account's codes with a new set and returns them (shown once, never stored).
// SECURITY: keyed digests only; the old set dies (test: regenerating_recovery_codes_voids_the_old_ones)
pub async fn regenerate(
    db: &impl ConnectionTrait,
    key: &[u8],
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<String>, AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    let codes: Vec<String> = (0..COUNT).map(|_| generate_one()).collect();
    for code in &codes {
        ActiveModel {
            user_id: Set(user_id),
            code_digest: Set(digest(key, code)),
            used_at: Set(None),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    Ok(codes)
}

/// Uses a code: marks it used if it is one of the account's unused codes.
// SECURITY: the conditional UPDATE (`used_at IS NULL`) is the claim, so a code works once even
// under concurrent requests (test: a_recovery_code_works_only_once, a_recovery_code_used_twice_at_the_same_time_works_once)
pub async fn redeem(
    db: &impl ConnectionTrait,
    key: &[u8],
    user_id: i64,
    code: &str,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    if normalize(code).len() != 10 {
        return Ok(false);
    }
    let res = Entity::update_many()
        .col_expr(Column::UsedAt, Expr::value(now))
        .filter(Column::UserId.eq(user_id))
        .filter(Column::CodeDigest.eq(digest(key, code)))
        .filter(Column::UsedAt.is_null())
        .exec(db)
        .await?;
    Ok(res.rows_affected == 1)
}

/// How many unused codes are left.
pub async fn remaining(db: &impl ConnectionTrait, user_id: i64) -> Result<u64, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user_id))
        .filter(Column::UsedAt.is_null())
        .count(db)
        .await?)
}

/// (unused, total) codes in the current set.
pub async fn counts(db: &impl ConnectionTrait, user_id: i64) -> Result<(u64, u64), AppError> {
    let total = Entity::find()
        .filter(Column::UserId.eq(user_id))
        .count(db)
        .await?;
    Ok((remaining(db, user_id).await?, total))
}

pub async fn remove_all(db: &impl ConnectionTrait, user_id: i64) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_readable_and_normalize() {
        let c = generate_one();
        assert_eq!(c.len(), 11);
        assert_eq!(&c[5..6], "-");
        assert!(
            c.chars()
                .all(|ch| ch == '-' || ALPHABET.contains(&(ch as u8)))
        );
        assert_eq!(normalize(" abcde-fghjk "), "ABCDEFGHJK");
        assert_eq!(digest(b"k", "abcde fghjk"), digest(b"k", "ABCDE-FGHJK"));
    }
}
