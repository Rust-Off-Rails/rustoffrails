//! Live rooms and who may join the members-only ones. The topic for a room is `room:<slug>`.

use chrono::{DateTime, Utc};
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};

use crate::error::AppError;
use crate::models::Errors;

pub use entity::{Column, Entity, Model};

pub mod entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "rooms")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub slug: String,
        pub name: String,
        pub headline: String,
        pub members_only: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod member {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "room_members")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub room_id: i64,
        pub person_id: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The topic every connection in this room joins.
pub fn topic(slug: &str) -> String {
    format!("room:{slug}")
}

pub async fn find_by_slug(
    db: &impl ConnectionTrait,
    slug: &str,
) -> Result<Option<Model>, AppError> {
    Ok(Entity::find().filter(Column::Slug.eq(slug)).one(db).await?)
}

/// May `person_id` be in this room? Open rooms: everyone signed in. Members-only rooms: the
/// people in `room_members`.
pub async fn admits(
    db: &impl ConnectionTrait,
    room: &Model,
    person_id: i64,
) -> Result<bool, AppError> {
    if !room.members_only {
        return Ok(true);
    }
    let n = member::Entity::find()
        .filter(member::Column::RoomId.eq(room.id))
        .filter(member::Column::PersonId.eq(person_id))
        .count(db)
        .await?;
    Ok(n > 0)
}

/// The room `slug`, if `person_id` may be in it; `None` for a missing room and a closed one alike,
/// so a slug reveals nothing.
pub async fn find_for(
    db: &impl ConnectionTrait,
    person_id: i64,
    slug: &str,
) -> Result<Option<Model>, AppError> {
    match find_by_slug(db, slug).await? {
        Some(room) if admits(db, &room, person_id).await? => Ok(Some(room)),
        _ => Ok(None),
    }
}

/// The rooms `person_id` may be in, by name.
pub async fn all_for(db: &impl ConnectionTrait, person_id: i64) -> Result<Vec<Model>, AppError> {
    let rooms = Entity::find().order_by_asc(Column::Id).all(db).await?;
    let mut visible = Vec::with_capacity(rooms.len());
    for room in rooms {
        if admits(db, &room, person_id).await? {
            visible.push(room);
        }
    }
    Ok(visible)
}

/// Adds a member (idempotent).
pub async fn add_member(
    db: &impl ConnectionTrait,
    room_id: i64,
    person_id: i64,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    member::Entity::insert(member::ActiveModel {
        id: NotSet,
        room_id: Set(room_id),
        person_id: Set(person_id),
        created_at: Set(now),
    })
    .on_conflict(
        OnConflict::columns([member::Column::RoomId, member::Column::PersonId])
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// Removes a member. The caller then calls `Hub::reauthorize(topic)`, so the person's open
/// connections leave the room at once.
pub async fn remove_member(
    db: &impl ConnectionTrait,
    room_id: i64,
    person_id: i64,
) -> Result<(), AppError> {
    member::Entity::delete_many()
        .filter(member::Column::RoomId.eq(room_id))
        .filter(member::Column::PersonId.eq(person_id))
        .exec(db)
        .await?;
    Ok(())
}

/// The headline form (untrusted).
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct HeadlineParams {
    #[serde(default)]
    pub headline: String,
}

impl HeadlineParams {
    pub fn validate(&self) -> Result<String, Errors> {
        let headline = self.headline.trim();
        let mut errors = Errors::default();
        if headline.chars().count() > 120 {
            errors.add("headline", "is too long (maximum is 120 characters)");
        } else if headline.chars().any(char::is_control) {
            errors.add("headline", "is invalid");
        }
        errors.into_result(|| headline.to_owned())
    }
}

pub async fn update_headline(
    db: &impl ConnectionTrait,
    room: Model,
    headline: String,
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    let mut active: entity::ActiveModel = room.into();
    active.headline = Set(headline);
    active.updated_at = Set(now);
    Ok(active.update(db).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headlines_are_trimmed_and_bounded() {
        let ok = HeadlineParams {
            headline: " Ship it ".into(),
        };
        assert_eq!(ok.validate().unwrap(), "Ship it");
        assert!(
            HeadlineParams {
                headline: "x".repeat(121)
            }
            .validate()
            .is_err()
        );
        assert_eq!(HeadlineParams::default().validate().unwrap(), "");
    }
}
