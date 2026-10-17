//! What people say in a room. A message's id is its `seq`: the cursor a client resumes from.

use chrono::{DateTime, Utc};
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde::Serialize;

use crate::error::AppError;
use crate::models::Errors;

pub use message::{Column, Entity, Model};

pub mod message {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "room_messages")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub room_id: i64,
        pub author_id: i64,
        pub author_name: String,
        #[sea_orm(column_type = "Text")]
        pub body: String,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The longest message, in characters. APP: your limit (frames are capped at 4 KiB anyway).
pub const MAX_BODY: usize = 1000;

/// What a client pushes (untrusted). Unknown fields are refused (`deny_unknown_fields`), so an
/// `author_id` in the payload is an error, never a way to post as someone else.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageParams {
    pub body: String,
}

impl MessageParams {
    /// Trims, keeps newlines and tabs, drops other control characters.
    pub fn validate(&self) -> Result<String, Errors> {
        let kept: String = self
            .body
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        let body = kept.trim().to_owned();
        let mut errors = Errors::default();
        if body.is_empty() {
            errors.add("body", "can't be blank");
        } else if body.chars().count() > MAX_BODY {
            errors.add("body", "is too long (maximum is 1000 characters)");
        }
        errors.into_result(|| body)
    }
}

/// A message as clients see it. The author is a display name: never an email.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MessageJson {
    pub id: i64,
    pub author_id: i64,
    pub author: String,
    pub initials: String,
    pub color: u8,
    pub body: String,
    pub at: String,
}

impl From<&Model> for MessageJson {
    fn from(m: &Model) -> Self {
        Self {
            id: m.id,
            author_id: m.author_id,
            author: m.author_name.clone(),
            initials: crate::models::guest::initials(&m.author_name),
            color: color_of(m.author_id),
            body: m.body.clone(),
            at: m.created_at.to_rfc3339(),
        }
    }
}

/// One of eight avatar colors, stable per person.
pub fn color_of(person_id: i64) -> u8 {
    (person_id.rem_euclid(8)) as u8
}

pub async fn create(
    db: &impl ConnectionTrait,
    room_id: i64,
    author_id: i64,
    author_name: &str,
    body: String,
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    Ok(message::ActiveModel {
        id: NotSet,
        room_id: Set(room_id),
        author_id: Set(author_id),
        author_name: Set(author_name.to_owned()),
        body: Set(body),
        created_at: Set(now),
    }
    .insert(db)
    .await?)
}

/// The room's messages after `since`, oldest first, at most `limit`.
pub async fn after(
    db: &impl ConnectionTrait,
    room_id: i64,
    since: i64,
    limit: u64,
) -> Result<Vec<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::RoomId.eq(room_id))
        .filter(Column::Id.gt(since))
        .order_by_asc(Column::Id)
        .limit(limit)
        .all(db)
        .await?)
}

/// The newest `limit` messages, oldest first (the page's first paint).
pub async fn latest(
    db: &impl ConnectionTrait,
    room_id: i64,
    limit: u64,
) -> Result<Vec<Model>, AppError> {
    let mut rows = Entity::find()
        .filter(Column::RoomId.eq(room_id))
        .order_by_desc(Column::Id)
        .limit(limit)
        .all(db)
        .await?;
    rows.reverse();
    Ok(rows)
}

/// The newest message id in the room (0 if none).
pub async fn high_water(db: &impl ConnectionTrait, room_id: i64) -> Result<i64, AppError> {
    Ok(Entity::find()
        .filter(Column::RoomId.eq(room_id))
        .order_by_desc(Column::Id)
        .one(db)
        .await?
        .map_or(0, |m| m.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_trimmed_bounded_and_stripped_of_control_characters() {
        let p = |body: &str| MessageParams { body: body.into() }.validate();
        assert_eq!(p("  hi\n there \u{7} ").unwrap(), "hi\n there");
        assert!(p("   ").is_err());
        assert!(p("\u{7}\u{8}").is_err());
        assert!(p(&"x".repeat(1001)).is_err());
        assert!(p(&"é".repeat(1000)).is_ok());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let bad = serde_json::from_str::<MessageParams>(r#"{"body":"hi","author_id":2}"#);
        assert!(bad.is_err());
    }
}
