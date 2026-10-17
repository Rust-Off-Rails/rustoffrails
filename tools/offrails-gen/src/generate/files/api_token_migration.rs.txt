//! Creates `api_tokens` (the first `cargo offrails generate api`): bearer tokens, digest only
//! (.offrails/guides/api.md#authentication-api-tokens).

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(ApiTokens::Table)
                .if_not_exists()
                .col(big_pk_auto(ApiTokens::Id))
                .col(big_integer(ApiTokens::UserId))
                .col(string(ApiTokens::Name))
                .col(string(ApiTokens::TokenDigest))
                .col(json(ApiTokens::Scopes))
                .col(timestamp_with_time_zone_null(ApiTokens::LastUsedAt))
                .col(timestamp_with_time_zone_null(ApiTokens::ExpiresAt))
                .col(timestamp_with_time_zone(ApiTokens::CreatedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("api_tokens_user_id_fkey")
                        .from(ApiTokens::Table, ApiTokens::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("api_tokens_token_digest_key")
                .table(ApiTokens::Table)
                .col(ApiTokens::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("api_tokens_user_id_idx")
                .table(ApiTokens::Table)
                .col(ApiTokens::UserId)
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(ApiTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ApiTokens {
    Table,
    Id,
    UserId,
    Name,
    TokenDigest,
    Scopes,
    LastUsedAt,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
