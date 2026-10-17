//! Demo identity for the realtime sample (`src/channels/identity.rs`): a guest is a display name,
//! a guest session is a cookie token, stored as a SHA-256 digest only. An app with accounts
//! points `identity.rs` at its own users and sessions instead (README, "Adapting it").

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Guests::Table)
                .if_not_exists()
                .col(big_pk_auto(Guests::Id))
                .col(string(Guests::Name))
                .col(timestamp_with_time_zone(Guests::CreatedAt))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(GuestSessions::Table)
                .if_not_exists()
                .col(big_pk_auto(GuestSessions::Id))
                .col(big_integer(GuestSessions::GuestId))
                .col(string(GuestSessions::TokenDigest))
                .col(timestamp_with_time_zone(GuestSessions::CreatedAt))
                .col(timestamp_with_time_zone(GuestSessions::LastSeenAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("guest_sessions_guest_id_fkey")
                        .from(GuestSessions::Table, GuestSessions::GuestId)
                        .to(Guests::Table, Guests::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("guest_sessions_token_digest_key")
                .table(GuestSessions::Table)
                .col(GuestSessions::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("guest_sessions_guest_id_idx")
                .table(GuestSessions::Table)
                .col(GuestSessions::GuestId)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(GuestSessions::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(Guests::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Guests {
    Table,
    Id,
    Name,
    CreatedAt,
}

#[derive(DeriveIden)]
enum GuestSessions {
    Table,
    Id,
    GuestId,
    TokenDigest,
    CreatedAt,
    LastSeenAt,
}
