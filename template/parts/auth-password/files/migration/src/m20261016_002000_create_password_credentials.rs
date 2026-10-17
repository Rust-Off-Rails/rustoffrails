//! `password_credentials`: one argon2id hash per account that has a password.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(PasswordCredentials::Table)
                .if_not_exists()
                .col(big_integer(PasswordCredentials::UserId).primary_key())
                .col(text(PasswordCredentials::Digest))
                .col(timestamp_with_time_zone(PasswordCredentials::ChangedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("password_credentials_user_id_fkey")
                        .from(PasswordCredentials::Table, PasswordCredentials::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(PasswordCredentials::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PasswordCredentials {
    Table,
    UserId,
    Digest,
    ChangedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
