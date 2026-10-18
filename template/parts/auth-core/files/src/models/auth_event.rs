//! The security audit trail: sign-ins, failures, credential and two-factor changes. Shown to the
//! person on their account page ("Recent security activity") and kept for investigations.
//!
//! GUIDE: .offrails/guides/security.md (logged with the acting user)
//!
//! Never stores a password, token, code or email body. A failed sign-in for an address with no
//! account is recorded without a user and without the address.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveValue::Set, QueryOrder, QuerySelect, entity::prelude::*};

use crate::{error::AppError, models::user};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "auth_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: Option<i64>,
    pub kind: Kind,
    pub ip: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    /// SHA-256 of the browser's `device` cookie: "is this a new device?" without storing it.
    pub device_digest: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Every kind any auth sample records. A kind no installed sample uses is harmless.
// APP: add kinds for your own sensitive actions (role changed, API token created, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(40))")]
pub enum Kind {
    #[sea_orm(string_value = "signed_in")]
    SignedIn,
    #[sea_orm(string_value = "sign_in_failed")]
    SignInFailed,
    #[sea_orm(string_value = "signed_out")]
    SignedOut,
    #[sea_orm(string_value = "session_revoked")]
    SessionRevoked,
    #[sea_orm(string_value = "other_sessions_revoked")]
    OtherSessionsRevoked,
    #[sea_orm(string_value = "reauthenticated")]
    Reauthenticated,
    #[sea_orm(string_value = "throttled")]
    Throttled,
    #[sea_orm(string_value = "signed_up")]
    SignedUp,
    #[sea_orm(string_value = "email_verified")]
    EmailVerified,
    #[sea_orm(string_value = "email_change_requested")]
    EmailChangeRequested,
    #[sea_orm(string_value = "email_changed")]
    EmailChanged,
    #[sea_orm(string_value = "password_changed")]
    PasswordChanged,
    #[sea_orm(string_value = "password_reset_requested")]
    PasswordResetRequested,
    #[sea_orm(string_value = "password_reset")]
    PasswordReset,
    #[sea_orm(string_value = "two_factor_enabled")]
    TwoFactorEnabled,
    #[sea_orm(string_value = "two_factor_disabled")]
    TwoFactorDisabled,
    #[sea_orm(string_value = "two_factor_failed")]
    TwoFactorFailed,
    #[sea_orm(string_value = "recovery_code_used")]
    RecoveryCodeUsed,
    #[sea_orm(string_value = "recovery_codes_regenerated")]
    RecoveryCodesRegenerated,
    #[sea_orm(string_value = "device_trusted")]
    DeviceTrusted,
    #[sea_orm(string_value = "not_me")]
    NotMe,
}

impl Kind {
    /// What the account page shows.
    // APP: wording on the account page's activity list.
    pub fn label(self) -> &'static str {
        match self {
            Kind::SignedIn => "Signed in",
            Kind::SignInFailed => "Failed sign-in attempt",
            Kind::SignedOut => "Signed out",
            Kind::SessionRevoked => "Signed out a device",
            Kind::OtherSessionsRevoked => "Signed out all other devices",
            Kind::Reauthenticated => "Confirmed identity",
            Kind::Throttled => "Sign-in paused after repeated failures",
            Kind::SignedUp => "Account created",
            Kind::EmailVerified => "Email address verified",
            Kind::EmailChangeRequested => "Email change requested",
            Kind::EmailChanged => "Email address changed",
            Kind::PasswordChanged => "Password changed",
            Kind::PasswordResetRequested => "Password reset requested",
            Kind::PasswordReset => "Password reset",
            Kind::TwoFactorEnabled => "Two-factor authentication turned on",
            Kind::TwoFactorDisabled => "Two-factor authentication turned off",
            Kind::TwoFactorFailed => "Wrong two-factor code",
            Kind::RecoveryCodeUsed => "Recovery code used",
            Kind::RecoveryCodesRegenerated => "New recovery codes generated",
            Kind::DeviceTrusted => "Device remembered for two-factor",
            Kind::NotMe => "Reported \"this wasn't me\": signed out everywhere",
        }
    }

    /// Shown with a warning tone.
    pub fn is_warning(self) -> bool {
        matches!(
            self,
            Kind::SignInFailed
                | Kind::Throttled
                | Kind::TwoFactorFailed
                | Kind::TwoFactorDisabled
                | Kind::NotMe
        )
    }
}

/// Who did it, from where. Built by the `RequestMeta` extractor.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub device_digest: Option<String>,
}

pub async fn record(
    db: &impl ConnectionTrait,
    user_id: Option<i64>,
    kind: Kind,
    meta: &Meta,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    // samples: auth events
    ActiveModel {
        user_id: Set(user_id),
        kind: Set(kind),
        ip: Set(meta.ip.clone()),
        user_agent: Set(meta
            .user_agent
            .as_ref()
            .map(|ua| ua.chars().take(400).collect())),
        device_digest: Set(meta.device_digest.clone()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// The person's latest events, newest first (the account page).
pub async fn recent_for(
    db: &impl ConnectionTrait,
    user: &user::Model,
    limit: u64,
) -> Result<Vec<Model>, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user.id))
        .order_by_desc(Column::CreatedAt)
        .order_by_desc(Column::Id)
        .limit(limit)
        .all(db)
        .await?)
}

/// Has this person signed in from this device before? Decides the "new sign-in" email.
pub async fn known_device(
    db: &impl ConnectionTrait,
    user_id: i64,
    device_digest: &str,
) -> Result<bool, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user_id))
        .filter(Column::Kind.eq(Kind::SignedIn))
        .filter(Column::DeviceDigest.eq(device_digest))
        .one(db)
        .await?
        .is_some())
}

/// Has this person ever signed in? (No "new device" email for the very first sign-in.)
pub async fn ever_signed_in(db: &impl ConnectionTrait, user_id: i64) -> Result<bool, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user_id))
        .filter(Column::Kind.eq(Kind::SignedIn))
        .one(db)
        .await?
        .is_some())
}

/// Events for one person, filtered by kind (tests and investigations).
pub async fn kinds_for(db: &impl ConnectionTrait, user_id: i64) -> Result<Vec<Kind>, AppError> {
    Ok(Entity::find()
        .filter(Column::UserId.eq(user_id))
        .order_by_asc(Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|e| e.kind)
        .collect())
}

/// For `app task auth_cleanup`: the trail is kept for a year.
// APP: retention. Check what your privacy policy promises.
pub async fn delete_older_than_a_year(
    db: &impl ConnectionTrait,
    now: DateTime<Utc>,
) -> Result<u64, AppError> {
    Ok(Entity::delete_many()
        .filter(Column::CreatedAt.lt(now - Duration::days(365)))
        .exec(db)
        .await?
        .rows_affected)
}
