//! Notes by slug (the public-show-by-slug snippet): `assign` gives a note a slug from its title
//! (unique: `-2`, `-3`… when taken), `find_public` finds one for the public page.
//! GUIDE: .offrails/template/snippets/public-show-by-slug/README.md

use sea_orm::{ActiveValue::Set, entity::prelude::*};

use super::{note, slug};
use crate::error::AppError;

/// Gives `record` a slug from its title, unless it has one (a URL, once shared, shouldn't move).
/// Call it right after `note::create` (APP: in the create handler, or in a backfill task).
#[tracing::instrument(skip_all, fields(id = record.id))]
pub async fn assign(
    db: &impl ConnectionTrait,
    record: note::Model,
) -> Result<note::Model, AppError> {
    if record.slug.is_some() {
        return Ok(record);
    }
    let fresh = slug::unique(&slug::slugify(&record.title), |s| async move {
        let n = note::Entity::find()
            .filter(note::Column::Slug.eq(s))
            .count(db)
            .await?;
        Ok::<_, AppError>(n > 0)
    })
    .await?;
    let mut am: note::ActiveModel = record.into();
    am.slug = Set(Some(fresh));
    Ok(am.update(db).await?)
}

/// The note at `/read/notes/{slug}`, for anyone. APP: only what may be public (a `published_at`
/// filter, if notes can be drafts).
#[tracing::instrument(skip_all)]
pub async fn find_public(db: &impl ConnectionTrait, slug: &str) -> Result<note::Model, AppError> {
    note::Entity::find()
        .filter(note::Column::Slug.eq(slug))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}
