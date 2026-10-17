//! Two-factor tables: `totp_credentials` (encrypted secrets), `recovery_codes` (keyed digests),
//! `trusted_devices` and `pending_logins` (token digests).

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

fn user_fk(
    table: impl IntoIden + Copy,
    column: impl IntoIden + Copy,
    name: &str,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(name)
        .from(table, column)
        .to(Users::Table, Users::Id)
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(TotpCredentials::Table)
                .if_not_exists()
                .col(big_integer(TotpCredentials::UserId).primary_key())
                .col(text(TotpCredentials::SecretCiphertext))
                .col(timestamp_with_time_zone_null(TotpCredentials::EnabledAt))
                .col(big_integer(TotpCredentials::LastUsedStep).default(0))
                .col(timestamp_with_time_zone(TotpCredentials::CreatedAt))
                .foreign_key(&mut user_fk(
                    TotpCredentials::Table,
                    TotpCredentials::UserId,
                    "totp_credentials_user_id_fkey",
                ))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(RecoveryCodes::Table)
                .if_not_exists()
                .col(big_pk_auto(RecoveryCodes::Id))
                .col(big_integer(RecoveryCodes::UserId))
                .col(string(RecoveryCodes::CodeDigest))
                .col(timestamp_with_time_zone_null(RecoveryCodes::UsedAt))
                .col(timestamp_with_time_zone(RecoveryCodes::CreatedAt))
                .foreign_key(&mut user_fk(
                    RecoveryCodes::Table,
                    RecoveryCodes::UserId,
                    "recovery_codes_user_id_fkey",
                ))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("recovery_codes_user_id_code_digest_idx")
                .table(RecoveryCodes::Table)
                .col(RecoveryCodes::UserId)
                .col(RecoveryCodes::CodeDigest)
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(TrustedDevices::Table)
                .if_not_exists()
                .col(big_pk_auto(TrustedDevices::Id))
                .col(big_integer(TrustedDevices::UserId))
                .col(string(TrustedDevices::TokenDigest))
                .col(string_null(TrustedDevices::Ip))
                .col(text_null(TrustedDevices::UserAgent))
                .col(timestamp_with_time_zone(TrustedDevices::CreatedAt))
                .col(timestamp_with_time_zone(TrustedDevices::LastUsedAt))
                .col(timestamp_with_time_zone(TrustedDevices::ExpiresAt))
                .foreign_key(&mut user_fk(
                    TrustedDevices::Table,
                    TrustedDevices::UserId,
                    "trusted_devices_user_id_fkey",
                ))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("trusted_devices_token_digest_key")
                .table(TrustedDevices::Table)
                .col(TrustedDevices::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("trusted_devices_user_id_idx")
                .table(TrustedDevices::Table)
                .col(TrustedDevices::UserId)
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(PendingLogins::Table)
                .if_not_exists()
                .col(big_pk_auto(PendingLogins::Id))
                .col(big_integer(PendingLogins::UserId))
                .col(string(PendingLogins::TokenDigest))
                .col(string(PendingLogins::Method))
                .col(integer(PendingLogins::Attempts).default(0))
                .col(timestamp_with_time_zone(PendingLogins::ExpiresAt))
                .col(timestamp_with_time_zone(PendingLogins::CreatedAt))
                .foreign_key(&mut user_fk(
                    PendingLogins::Table,
                    PendingLogins::UserId,
                    "pending_logins_user_id_fkey",
                ))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("pending_logins_token_digest_key")
                .table(PendingLogins::Table)
                .col(PendingLogins::TokenDigest)
                .unique()
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for t in [
            PendingLogins::Table.into_iden(),
            TrustedDevices::Table.into_iden(),
            RecoveryCodes::Table.into_iden(),
            TotpCredentials::Table.into_iden(),
        ] {
            m.drop_table(Table::drop().table(t).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum TotpCredentials {
    Table,
    UserId,
    SecretCiphertext,
    EnabledAt,
    LastUsedStep,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum RecoveryCodes {
    Table,
    Id,
    UserId,
    CodeDigest,
    UsedAt,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum TrustedDevices {
    Table,
    Id,
    UserId,
    TokenDigest,
    Ip,
    UserAgent,
    CreatedAt,
    LastUsedAt,
    ExpiresAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum PendingLogins {
    Table,
    Id,
    UserId,
    TokenDigest,
    Method,
    Attempts,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum Users {
    Table,
    Id,
}
