//! The dev mailbox (`kernel/mailbox.rs`): every email `Mailer::Local` delivers, in development.
//! The table exists in every environment so the schema is the same everywhere.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(MailboxMessages::Table)
                .if_not_exists()
                .col(big_pk_auto(MailboxMessages::Id))
                .col(string(MailboxMessages::ToAddr))
                .col(string(MailboxMessages::FromAddr))
                .col(json(MailboxMessages::Cc).default("[]"))
                .col(json(MailboxMessages::Bcc).default("[]"))
                .col(string_null(MailboxMessages::ReplyTo))
                .col(string(MailboxMessages::Subject))
                .col(text(MailboxMessages::Html))
                .col(text(MailboxMessages::Text))
                .col(json(MailboxMessages::Headers))
                .col(text(MailboxMessages::Eml))
                .col(json(MailboxMessages::Attachments).default("[]"))
                .col(timestamp_with_time_zone_null(MailboxMessages::ReadAt))
                .col(timestamp_with_time_zone(MailboxMessages::CreatedAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("mailbox_messages_to_addr_idx")
                .table(MailboxMessages::Table)
                .col(MailboxMessages::ToAddr)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(MailboxMessages::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum MailboxMessages {
    Table,
    Id,
    ToAddr,
    FromAddr,
    Cc,
    Bcc,
    ReplyTo,
    Subject,
    Html,
    Text,
    Headers,
    Eml,
    Attachments,
    ReadAt,
    CreatedAt,
}
