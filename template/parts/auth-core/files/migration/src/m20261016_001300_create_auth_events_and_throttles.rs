//! `auth_events`: the security audit trail, and `auth_throttles`: progressive delays for guesses.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(AuthEvents::Table)
                .if_not_exists()
                .col(big_pk_auto(AuthEvents::Id))
                .col(big_integer_null(AuthEvents::UserId))
                .col(string_len(AuthEvents::Kind, 40))
                .col(string_null(AuthEvents::Ip))
                .col(text_null(AuthEvents::UserAgent))
                .col(string_null(AuthEvents::DeviceDigest))
                .col(timestamp_with_time_zone(AuthEvents::CreatedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("auth_events_user_id_fkey")
                        .from(AuthEvents::Table, AuthEvents::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("auth_events_user_id_created_at_idx")
                .table(AuthEvents::Table)
                .col(AuthEvents::UserId)
                .col(AuthEvents::CreatedAt)
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(AuthThrottles::Table)
                .if_not_exists()
                .col(string(AuthThrottles::Key).primary_key())
                .col(integer(AuthThrottles::Failures))
                .col(timestamp_with_time_zone_null(AuthThrottles::BlockedUntil))
                .col(timestamp_with_time_zone(AuthThrottles::UpdatedAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("auth_throttles_updated_at_idx")
                .table(AuthThrottles::Table)
                .col(AuthThrottles::UpdatedAt)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(AuthThrottles::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(AuthEvents::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AuthEvents {
    Table,
    Id,
    UserId,
    Kind,
    Ip,
    UserAgent,
    DeviceDigest,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AuthThrottles {
    Table,
    Key,
    Failures,
    BlockedUntil,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
