//! Socket tokens are single use (`kernel/socket_token.rs`): each redeemed token's nonce digest is
//! kept until the token would have expired, on every node.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(RealtimeTokenUses::Table)
                .if_not_exists()
                .col(string(RealtimeTokenUses::NonceDigest).primary_key())
                .col(timestamp_with_time_zone(RealtimeTokenUses::ExpiresAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("realtime_token_uses_expires_at_idx")
                .table(RealtimeTokenUses::Table)
                .col(RealtimeTokenUses::ExpiresAt)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(RealtimeTokenUses::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RealtimeTokenUses {
    Table,
    NonceDigest,
    ExpiresAt,
}
