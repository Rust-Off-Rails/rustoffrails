//! Adds `notes.slug` (the public-show-by-slug snippet): nullable, so existing rows need no value
//! (`note_slugs::assign` gives them one), and unique: two notes never share a URL.
//! One ALTER per column: SQLite runs one change per statement.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Notes::Table)
                .add_column(string_null(Notes::Slug))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("notes_slug_key")
                .table(Notes::Table)
                .col(Notes::Slug)
                .unique()
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_index(
            Index::drop()
                .name("notes_slug_key")
                .table(Notes::Table)
                .to_owned(),
        )
        .await?;
        m.alter_table(
            Table::alter()
                .table(Notes::Table)
                .drop_column(Notes::Slug)
                .to_owned(),
        )
        .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Notes {
    Table,
    Slug,
}
