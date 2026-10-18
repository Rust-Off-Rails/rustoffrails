//! Notes queries. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use chrono::Utc;
use sea_orm::{ActiveValue::Set, QueryOrder, QuerySelect, entity::prelude::*};

use super::{ActiveModel, Column, Entity, Model, NewNote};
use crate::error::AppError;
use crate::models::Page;

/// Public: anyone may read any note (`--public`; .offrails/guides/models.md#scoped-lookups-tenancy).
#[tracing::instrument(skip_all, fields(id))]
pub async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {
    Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// A page of notes, newest first, and whether there's another page.
#[tracing::instrument(skip_all)]
pub async fn all(db: &impl ConnectionTrait, page: Page) -> Result<(Vec<Model>, bool), AppError> {
    let per = page.per();
    let mut rows = Entity::find()
        .order_by_desc(Column::CreatedAt)
        .order_by_desc(Column::Id)
        .limit(per + 1)
        .offset(page.offset())
        .all(db)
        .await?;
    let more = rows.len() as u64 > per;
    rows.truncate(per as usize);
    Ok((rows, more))
}

/// Notes for a `<select>`: (id, label), at most 200, by label.
#[tracing::instrument(skip_all)]
pub async fn choices(db: &impl ConnectionTrait) -> Result<Vec<(i64, String)>, AppError> {
    let rows = Entity::find()
        .order_by_desc(Column::Id)
        .limit(200)
        .all(db)
        .await?;
    let mut out: Vec<(i64, String)> = rows.iter().map(|r| (r.id, r.label())).collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

#[tracing::instrument(skip_all)]
pub async fn create(db: &impl ConnectionTrait, new: NewNote) -> Result<Model, AppError> {
    let now = Utc::now();
    ActiveModel {
        title: Set(new.title),
        body: Set(new.body),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(AppError::from)
}

#[tracing::instrument(skip_all, fields(id = record.id))]
pub async fn update(
    db: &impl ConnectionTrait,
    record: Model,
    new: NewNote,
) -> Result<Model, AppError> {
    let mut am: ActiveModel = record.into();
    am.title = Set(new.title);
    am.body = Set(new.body);
    am.updated_at = Set(Utc::now());
    am.update(db).await.map_err(AppError::from)
}

#[tracing::instrument(skip_all, fields(id = record.id))]
pub async fn destroy(db: &impl ConnectionTrait, record: Model) -> Result<(), AppError> {
    Entity::delete_by_id(record.id).exec(db).await?;
    Ok(())
}
