//! `login_links`: emailed sign-in links and codes (digests only), bound to the asking browser.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(LoginLinks::Table)
                .if_not_exists()
                .col(big_pk_auto(LoginLinks::Id))
                .col(string(LoginLinks::Email))
                .col(string_len(LoginLinks::Purpose, 16))
                .col(string(LoginLinks::TokenDigest))
                .col(string(LoginLinks::CodeDigest))
                .col(string(LoginLinks::BrowserDigest))
                .col(integer(LoginLinks::CodeAttempts).default(0))
                .col(timestamp_with_time_zone(LoginLinks::ExpiresAt))
                .col(timestamp_with_time_zone(LoginLinks::CreatedAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("login_links_token_digest_key")
                .table(LoginLinks::Table)
                .col(LoginLinks::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("login_links_browser_digest_idx")
                .table(LoginLinks::Table)
                .col(LoginLinks::BrowserDigest)
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("login_links_email_idx")
                .table(LoginLinks::Table)
                .col(LoginLinks::Email)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(LoginLinks::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum LoginLinks {
    Table,
    Id,
    Email,
    Purpose,
    TokenDigest,
    CodeDigest,
    BrowserDigest,
    CodeAttempts,
    ExpiresAt,
    CreatedAt,
}
