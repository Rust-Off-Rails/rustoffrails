use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(CacheEntries::Table)
                .if_not_exists()
                .col(string(CacheEntries::Key).primary_key())
                .col(text(CacheEntries::Value))
                .col(timestamp_with_time_zone(CacheEntries::ExpiresAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("cache_entries_expires_at_idx")
                .table(CacheEntries::Table)
                .col(CacheEntries::ExpiresAt)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(CacheEntries::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CacheEntries {
    Table,
    Key,
    Value,
    ExpiresAt,
}
