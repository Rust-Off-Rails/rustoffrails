//! `sessions`: one row per signed-in browser; the cookie holds a token, this holds its digest.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Sessions::Table)
                .if_not_exists()
                .col(big_pk_auto(Sessions::Id))
                .col(big_integer(Sessions::UserId))
                .col(string(Sessions::TokenDigest))
                .col(string_null(Sessions::Ip))
                .col(text_null(Sessions::UserAgent))
                .col(string(Sessions::SignInMethod))
                .col(timestamp_with_time_zone(Sessions::AuthenticatedAt))
                .col(timestamp_with_time_zone(Sessions::CreatedAt))
                .col(timestamp_with_time_zone(Sessions::LastSeenAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("sessions_user_id_fkey")
                        .from(Sessions::Table, Sessions::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("sessions_token_digest_key")
                .table(Sessions::Table)
                .col(Sessions::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("sessions_user_id_idx")
                .table(Sessions::Table)
                .col(Sessions::UserId)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    TokenDigest,
    Ip,
    UserAgent,
    SignInMethod,
    AuthenticatedAt,
    CreatedAt,
    LastSeenAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
