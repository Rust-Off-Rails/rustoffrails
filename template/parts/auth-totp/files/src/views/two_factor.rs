//! Page structs for two-factor: the code challenge, enrolment (QR + manual key), recovery codes,
//! and the account-page summary.

use askama::Template;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;

use super::Ctx;
use crate::error::AppError;
use crate::models::{Errors, recovery_code, totp_credential, trusted_device, user};
use crate::routes::paths;

/// `GET /two-factor`: "Enter the code from your authenticator app".
#[derive(Template)]
#[template(path = "two_factor/challenge.html")]
pub struct TwoFactorChallenge {
    pub ctx: Ctx,
    pub alert: Option<String>,
    pub errors: Errors,
    /// "code" or "recovery": which form is open.
    pub mode: &'static str,
}

/// `GET /account/two-factor/new`: scan the QR code, then confirm with a code.
#[derive(Template)]
#[template(path = "two_factor/new.html")]
pub struct TwoFactorNew {
    pub ctx: Ctx,
    /// `data:image/svg+xml;base64,…`, generated server-side: the secret never goes to a
    /// third-party QR service.
    pub qr_data_uri: String,
    pub manual_key: String,
    pub account: String,
    pub issuer: String,
    pub errors: Errors,
    /// Development demo only: the obviously fake all-zero secret.
    pub demo_secret: bool,
}

/// The recovery codes, shown once (after enabling, or after generating a new set).
#[derive(Template)]
#[template(path = "two_factor/recovery_codes.html")]
pub struct RecoveryCodes {
    pub ctx: Ctx,
    pub codes: Vec<String>,
    /// `data:text/plain;base64,…` for the download link (no request carries the codes again).
    pub download_href: String,
    pub just_enabled: bool,
}

impl RecoveryCodes {
    pub fn new(ctx: Ctx, codes: Vec<String>, account: &str, just_enabled: bool) -> Self {
        let app = crate::config::APP_NAME;
        let text = format!(
            "{app} recovery codes for {account}\n\nEach code works once. Keep them somewhere safe, like a password manager.\n\n{}\n",
            codes.join("\n")
        );
        Self {
            ctx,
            download_href: format!(
                "data:text/plain;charset=utf-8;base64,{}",
                STANDARD.encode(text)
            ),
            codes,
            just_enabled,
        }
    }
}

/// A remembered device on the account page.
pub struct TrustedRow {
    pub id: i64,
    pub device: String,
    pub last_used: String,
    pub expires: String,
}

/// The two-factor card on the account page.
pub struct TwoFactorSummary {
    pub enabled: bool,
    pub recovery_left: u64,
    pub recovery_total: u64,
    pub trusted: Vec<TrustedRow>,
}

impl TwoFactorSummary {
    pub async fn load(
        db: &impl ConnectionTrait,
        user: &user::Model,
        now: DateTime<Utc>,
    ) -> Result<Self, AppError> {
        let enabled = totp_credential::enabled_for(db, user.id).await?;
        let trusted = trusted_device::all_for(db, user, now)
            .await?
            .iter()
            .map(|d| TrustedRow {
                id: d.id,
                device: crate::kernel::user_agent::describe(d.user_agent.as_deref()),
                last_used: crate::views::auth::time_ago(d.last_used_at, now),
                expires: d.expires_at.format("%b %-d").to_string(),
            })
            .collect();
        let (recovery_left, recovery_total) = if enabled {
            recovery_code::counts(db, user.id).await?
        } else {
            (0, 0)
        };
        Ok(Self {
            enabled,
            recovery_left,
            recovery_total,
            trusted,
        })
    }

    pub fn recovery_low(&self) -> bool {
        self.enabled && self.recovery_left <= 3
    }

    pub fn has_recovery_codes(&self) -> bool {
        self.recovery_total > 0
    }

    pub fn recovery_used(&self) -> u64 {
        self.recovery_total.saturating_sub(self.recovery_left)
    }
}

/// The otpauth:// URI as a QR code, an SVG data URI.
pub fn qr_data_uri(otpauth: &str) -> Result<String, AppError> {
    let code = qrcode::QrCode::new(otpauth.as_bytes())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("qr: {e}")))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(qrcode::render::svg::Color("#111827"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(svg)
    ))
}
