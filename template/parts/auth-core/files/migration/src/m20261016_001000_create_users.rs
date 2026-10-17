//! `users`: who the person is. Credentials live in each sign-in method's own table.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Users::Table)
                .if_not_exists()
                .col(big_pk_auto(Users::Id))
                .col(string(Users::Email))
                .col(timestamp_with_time_zone_null(Users::EmailVerifiedAt))
                .col(timestamp_with_time_zone(Users::CreatedAt))
                .col(timestamp_with_time_zone(Users::UpdatedAt))
                .to_owned(),
        )
        .await?;
        // SECURITY: one account per address, enforced by the database (no check-then-insert race)
        // (test: find_or_create_twice_in_one_transaction_keeps_it_usable)
        m.create_index(
            Index::create()
                .name("users_email_key")
                .table(Users::Table)
                .col(Users::Email)
                .unique()
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Email,
    EmailVerifiedAt,
    CreatedAt,
    UpdatedAt,
}
