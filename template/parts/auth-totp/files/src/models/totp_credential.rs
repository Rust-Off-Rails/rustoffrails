//! TOTP two-factor authentication (RFC 6238): HMAC-SHA-1, 30-second steps, 6 digits, one step of
//! clock drift either way. The secret is 160 random bits, encrypted at rest (`kernel::secret_box`).
//!
//! - Enrolment creates a *pending* row (`enabled_at` NULL); it only protects the account once a
//!   code from the app confirms it ("confirm before enable").
//! - Replay protection: the last accepted time step is stored and a code is only accepted for a
//!   **later** step, claimed with a conditional UPDATE, so the same code can't be used twice, even
//!   by two racing requests.

use chrono::{DateTime, Utc};
use sea_orm::{ActiveValue::Set, entity::prelude::*, sea_query::Expr};
use totp_rs::{Algorithm, Builder, Secret, Totp};

use crate::{config::TotpConfig, error::AppError, kernel::auth_token, kernel::secret_box};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "totp_credentials")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
    /// `v1:<nonce+ciphertext>` (never the secret in the clear).
    #[sea_orm(column_type = "Text")]
    pub secret_ciphertext: String,
    /// `None` until a code confirmed the enrolment.
    pub enabled_at: Option<DateTimeUtc>,
    /// The last time step a code was accepted for (replay protection).
    pub last_used_step: i64,
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

/// RFC 6238 parameters every authenticator app supports.
// WHY: SHA-1/30 s/6 digits is what Google Authenticator, 1Password, Authy and others agree on;
// "stronger" settings are silently ignored by some apps, which then show wrong codes.
pub const STEP_SECS: u64 = 30;
pub const DIGITS: usize = 6;
/// One step of drift either way (RFC 6238 §5.2).
pub const SKEW: u64 = 1;

/// The fixed, obviously fake demo secret (20 zero bytes, `AAAA…` in base32). Only the
/// development demo task uses it; enrolment always generates a random one.
pub const DEMO_SECRET: [u8; 20] = [0u8; 20];

/// A TOTP for `secret`, labelled for authenticator apps.
pub fn totp(secret: &[u8], account: &str) -> Result<Totp, AppError> {
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(DIGITS as u8)
        .with_step_duration(STEP_SECS)
        .with_skew(SKEW as u16)
        .with_secret(Secret::new(secret.to_vec().into_boxed_slice()))
        // APP: the issuer shown in authenticator apps (no ':' allowed).
        .with_issuer(Some(crate::config::APP_NAME.replace(':', "")))
        .with_account_name(account.replace(':', ""))
        .build()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("totp: {e}")))
}

/// The time step a code matches, within ±`SKEW` of `now`, considering only steps **after**
/// `last_used`. Constant-time comparison of each candidate.
// SECURITY: a code for an already-used (or older) step is refused, so a shoulder-surfed or
// phished code can't be replayed (test: a_totp_code_works_only_once)
pub fn matching_step(totp: &Totp, code: &str, now: u64, last_used: i64) -> Option<i64> {
    let code = auth_token::digits_only(code);
    if code.len() != DIGITS {
        return None;
    }
    let current = now / STEP_SECS;
    let mut found = None;
    // WHY: every candidate is computed and compared (no early exit), so timing doesn't tell
    // which step matched.
    for step in current.saturating_sub(SKEW)..=current + SKEW {
        let expected = totp.generate(step * STEP_SECS).to_string();
        let same = auth_token::constant_time_eq(expected.as_bytes(), code.as_bytes());
        if same && (step as i64) > last_used && found.is_none() {
            found = Some(step as i64);
        }
    }
    found
}

/// A random 160-bit secret (RFC 4226 §4 recommends 160).
pub fn generate_secret() -> [u8; 20] {
    let mut s = [0u8; 20];
    rand::fill(&mut s);
    s
}

/// The account's credential, enabled or pending.
pub async fn find_for(db: &impl ConnectionTrait, user_id: i64) -> Result<Option<Model>, AppError> {
    Ok(Entity::find_by_id(user_id).one(db).await?)
}

/// Is two-factor on for this account?
pub async fn enabled_for(db: &impl ConnectionTrait, user_id: i64) -> Result<bool, AppError> {
    Ok(find_for(db, user_id)
        .await?
        .is_some_and(|c| c.enabled_at.is_some()))
}

/// Decrypts the secret (current key, then the previous one during a rotation).
pub fn secret_of(config: &TotpConfig, cred: &Model) -> Result<Vec<u8>, AppError> {
    let keys: Vec<&[u8; 32]> = std::iter::once(&config.key)
        .chain(config.previous_key.as_ref())
        .collect();
    secret_box::open(&keys, cred.user_id, &cred.secret_ciphertext)
        .map(|(plain, _)| plain)
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "totp secret can't be decrypted (wrong APP_TOTP_ENCRYPTION_KEY?)"
            ))
        })
}

/// The pending (unconfirmed) credential, created with a fresh secret if there is none. Reloading
/// the enrolment page keeps the same secret, so a scanned QR code stays valid.
pub async fn pending_for(
    db: &impl ConnectionTrait,
    config: &TotpConfig,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    if let Some(c) = find_for(db, user_id).await?
        && c.enabled_at.is_none()
    {
        return Ok(c);
    }
    replace_pending(db, config, user_id, &generate_secret(), now).await
}

/// Stores a pending credential with `secret` (enrolment, and the development demo).
pub async fn replace_pending(
    db: &impl ConnectionTrait,
    config: &TotpConfig,
    user_id: i64,
    secret: &[u8],
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .filter(Column::EnabledAt.is_null())
        .exec(db)
        .await?;
    let sealed = secret_box::seal(&config.key, user_id, secret)?;
    Ok(ActiveModel {
        user_id: Set(user_id),
        secret_ciphertext: Set(sealed),
        enabled_at: Set(None),
        last_used_step: Set(0),
        created_at: Set(now),
    }
    .insert(db)
    .await?)
}

/// Checks `code` and, if right, claims its step so it can't be used again. Works for pending
/// credentials too (enrolment confirmation).
// SECURITY: the claim is `UPDATE … WHERE last_used_step < step`; of two requests racing with the
// same code, exactly one wins (test: a_totp_code_works_only_once)
pub async fn verify(
    db: &impl ConnectionTrait,
    config: &TotpConfig,
    cred: &Model,
    account: &str,
    code: &str,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let secret = secret_of(config, cred)?;
    let totp = totp(&secret, account)?;
    let Some(step) = matching_step(
        &totp,
        code,
        now.timestamp().max(0) as u64,
        cred.last_used_step,
    ) else {
        return Ok(false);
    };
    let claimed = Entity::update_many()
        .col_expr(Column::LastUsedStep, Expr::value(step))
        .filter(Column::UserId.eq(cred.user_id))
        .filter(Column::LastUsedStep.lt(step))
        .exec(db)
        .await?;
    Ok(claimed.rows_affected == 1)
}

/// Turns the confirmed pending credential on.
pub async fn enable(
    db: &impl ConnectionTrait,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    Entity::update_many()
        .col_expr(Column::EnabledAt, Expr::value(now))
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Turns two-factor off: removes the credential.
pub async fn remove(db: &impl ConnectionTrait, user_id: i64) -> Result<(), AppError> {
    Entity::delete_many()
        .filter(Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Re-encrypts the secret under the current key (after a rotation).
pub async fn reseal(
    db: &impl ConnectionTrait,
    config: &TotpConfig,
    cred: &Model,
) -> Result<(), AppError> {
    let secret = secret_of(config, cred)?;
    let sealed = secret_box::seal(&config.key, cred.user_id, &secret)?;
    Entity::update_many()
        .col_expr(Column::SecretCiphertext, Expr::value(sealed))
        .filter(Column::UserId.eq(cred.user_id))
        .exec(db)
        .await?;
    Ok(())
}

/// `ABCD EFGH IJKL …`: the base32 secret in groups of four, for typing into an app.
pub fn manual_key(secret: &[u8]) -> String {
    let b32 = Secret::new(secret.to_vec().into_boxed_slice()).to_base32();
    b32.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B test vectors (public values from the RFC, not secrets).
    const RFC_SHA1_SEED: &[u8] = b"12345678901234567890"; // gitleaks:allow
    const RFC_SHA256_SEED: &[u8] = b"12345678901234567890123456789012"; // gitleaks:allow
    const RFC_SHA512_SEED: &[u8] =
        b"1234567890123456789012345678901234567890123456789012345678901234"; // gitleaks:allow

    fn rfc(alg: Algorithm, seed: &[u8]) -> Totp {
        Builder::new()
            .with_algorithm(alg)
            .with_digits(8)
            .with_step_duration(30)
            .with_secret(Secret::new(seed.to_vec().into_boxed_slice()))
            .build()
            .unwrap()
    }

    #[test]
    fn rfc6238_test_vectors() {
        let table: &[(u64, &str, &str, &str)] = &[
            (59, "94287082", "46119246", "90693936"),
            (1111111109, "07081804", "68084774", "25091201"),
            (1111111111, "14050471", "67062674", "99943326"),
            (1234567890, "89005924", "91819424", "93441116"),
            (2000000000, "69279037", "90698825", "38618901"),
            (20000000000, "65353130", "77737706", "47863826"),
        ];
        let (s1, s256, s512) = (
            rfc(Algorithm::SHA1, RFC_SHA1_SEED),
            rfc(Algorithm::SHA256, RFC_SHA256_SEED),
            rfc(Algorithm::SHA512, RFC_SHA512_SEED),
        );
        for (t, sha1, sha256, sha512) in table {
            assert_eq!(s1.generate(*t).to_string(), *sha1, "SHA-1 at {t}");
            assert_eq!(s256.generate(*t).to_string(), *sha256, "SHA-256 at {t}");
            assert_eq!(s512.generate(*t).to_string(), *sha512, "SHA-512 at {t}");
        }
        // Our 6-digit SHA-1 codes are the last 6 digits of the RFC's 8-digit ones.
        let six = totp(RFC_SHA1_SEED, "rfc@example.com").unwrap();
        assert_eq!(six.generate(59).to_string(), "287082");
        assert_eq!(six.generate(1111111109).to_string(), "081804");
    }

    #[test]
    fn codes_match_within_one_step_and_never_an_old_step() {
        let t = totp(RFC_SHA1_SEED, "a@example.com").unwrap();
        let now = 1_111_111_111u64; // step 37037037
        let step = (now / 30) as i64;
        let code_now = t.generate(now).to_string();
        assert_eq!(matching_step(&t, &code_now, now, 0), Some(step));
        let code_prev = t.generate(now - 30).to_string();
        assert_eq!(matching_step(&t, &code_prev, now, 0), Some(step - 1));
        let code_next = t.generate(now + 30).to_string();
        assert_eq!(matching_step(&t, &code_next, now, 0), Some(step + 1));
        let too_old = t.generate(now - 90).to_string();
        assert_eq!(matching_step(&t, &too_old, now, 0), None);
        // Replay: the step was used already.
        assert_eq!(matching_step(&t, &code_now, now, step), None);
        assert_eq!(matching_step(&t, &code_prev, now, step), None);
        assert_eq!(matching_step(&t, "12345", now, 0), None);
        assert_eq!(matching_step(&t, "abcdef", now, 0), None);
        let spaced = format!("{} {}", &code_now[..3], &code_now[3..]);
        assert_eq!(matching_step(&t, &spaced, now, 0), Some(step));
    }

    #[test]
    fn the_manual_key_is_grouped_base32() {
        assert_eq!(
            manual_key(&DEMO_SECRET),
            "AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA"
        );
    }
}
