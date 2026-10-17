//! `user_tokens`: single-use emailed tokens (verification, reset, email change, "not me").

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(UserTokens::Table)
                .if_not_exists()
                .col(big_pk_auto(UserTokens::Id))
                .col(big_integer(UserTokens::UserId))
                .col(string_len(UserTokens::Purpose, 32))
                .col(string(UserTokens::TokenDigest))
                .col(string(UserTokens::SentTo))
                .col(string_null(UserTokens::BrowserDigest))
                .col(timestamp_with_time_zone(UserTokens::ExpiresAt))
                .col(timestamp_with_time_zone(UserTokens::CreatedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("user_tokens_user_id_fkey")
                        .from(UserTokens::Table, UserTokens::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("user_tokens_token_digest_key")
                .table(UserTokens::Table)
                .col(UserTokens::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("user_tokens_user_id_purpose_idx")
                .table(UserTokens::Table)
                .col(UserTokens::UserId)
                .col(UserTokens::Purpose)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(UserTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UserTokens {
    Table,
    Id,
    UserId,
    Purpose,
    TokenDigest,
    SentTo,
    BrowserDigest,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
