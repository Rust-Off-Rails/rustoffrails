//! Creates `notes`. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).
//! APP: committed migrations are append-only: change the table with a new migration
//! (.offrails/guides/migrations.md#rules).

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Notes::Table)
                .if_not_exists()
                .col(big_pk_auto(Notes::Id))
                .col(string(Notes::Title))
                .col(text(Notes::Body))
                .col(timestamp_with_time_zone(Notes::CreatedAt))
                .col(timestamp_with_time_zone(Notes::UpdatedAt))
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(Notes::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Notes {
    Table,
    Id,
    Title,
    Body,
    CreatedAt,
    UpdatedAt,
}
