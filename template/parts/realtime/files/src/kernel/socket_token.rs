//! Short-lived signed socket tokens, for clients that can't send the session cookie: a frontend on
//! another origin, a native app, a script.
//!
//! `v1.<person>.<session>.<expires>.<nonce>.<signature>`: HMAC-SHA256 under
//! `APP_REALTIME_SECRET`, valid for `TTL`, redeemable **once** (the nonce's digest is stored until
//! the token would have expired, so a replay fails on every node), and bound to the session that
//! minted it: a socket opened with a token is closed when that session ends, like any other.
//!
//! The client offers it as a second WebSocket subprotocol, `offrails.token.<token>` (never in the
//! URL, where it would land in access logs). Cookies are ignored on such a handshake.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Set};
use sha2::{Digest, Sha256};

/// How long a token may wait before it is redeemed. APP: longer for slow clients, never hours.
pub const TTL: Duration = Duration::seconds(60);
/// `APP_REALTIME_SECRET` must be at least this long.
pub const MIN_SECRET: usize = 32;

const DOMAIN: &[u8] = b"offrails.v1.socket-token|";

pub mod token_use {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "realtime_token_uses")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub nonce_digest: String,
        pub expires_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// What a valid token says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub person_id: i64,
    pub session_id: i64,
    pub expires_at: i64,
    pub nonce: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed socket token")]
    Malformed,
    #[error("bad socket token signature")]
    Signature,
    #[error("socket token expired")]
    Expired,
    #[error("socket token already used")]
    Replayed,
    #[error(transparent)]
    Db(#[from] DbErrString),
}

/// `DbErr` isn't `PartialEq`; tests compare errors.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DbErrString(String);

impl From<DbErr> for TokenError {
    fn from(e: DbErr) -> Self {
        TokenError::Db(DbErrString(e.to_string()))
    }
}

fn mac(secret: &[u8], payload: &str) -> Option<Hmac<Sha256>> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).ok()?;
    mac.update(DOMAIN);
    mac.update(payload.as_bytes());
    Some(mac)
}

/// A token for this person's session, valid for `TTL` from `now`.
pub fn mint(secret: &[u8], person_id: i64, session_id: i64, now: DateTime<Utc>) -> String {
    let nonce = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());
    let expires = (now + TTL).timestamp();
    let payload = format!("v1.{person_id}.{session_id}.{expires}.{nonce}");
    let signature = mac(secret, &payload)
        .map(|m| URL_SAFE_NO_PAD.encode(m.finalize().into_bytes()))
        .unwrap_or_default();
    format!("{payload}.{signature}")
}

/// Checks format, signature (in constant time) and expiry. Doesn't redeem: see `redeem`.
pub fn verify(secret: &[u8], token: &str, now: DateTime<Utc>) -> Result<Claims, TokenError> {
    if token.len() > 256 {
        return Err(TokenError::Malformed);
    }
    let (payload, signature) = token.rsplit_once('.').ok_or(TokenError::Malformed)?;
    let parts: Vec<&str> = payload.split('.').collect();
    let [version, person, session, expires, nonce] = parts[..] else {
        return Err(TokenError::Malformed);
    };
    if version != "v1" || nonce.is_empty() {
        return Err(TokenError::Malformed);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| TokenError::Malformed)?;
    // SECURITY: the signature is checked before anything in the payload is trusted, in constant time (test: a_forged_or_altered_socket_token_is_refused).
    mac(secret, payload)
        .ok_or(TokenError::Signature)?
        .verify_slice(&signature)
        .map_err(|_| TokenError::Signature)?;
    let claims = Claims {
        person_id: person.parse().map_err(|_| TokenError::Malformed)?,
        session_id: session.parse().map_err(|_| TokenError::Malformed)?,
        expires_at: expires.parse().map_err(|_| TokenError::Malformed)?,
        nonce: nonce.to_owned(),
    };
    let now = now.timestamp();
    // SECURITY: short-lived, and never valid for longer than TTL even if signed so (test: an_expired_socket_token_is_refused).
    if claims.expires_at <= now || claims.expires_at > now + TTL.num_seconds() + 5 {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

/// Marks the token used. A second redemption, on any node, is `Replayed`.
pub async fn redeem(
    db: &impl ConnectionTrait,
    claims: &Claims,
    now: DateTime<Utc>,
) -> Result<(), TokenError> {
    let digest: String = Sha256::digest(claims.nonce.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // Housekeeping first: expired uses can never matter again.
    token_use::Entity::delete_many()
        .filter(token_use::Column::ExpiresAt.lt(now))
        .exec(db)
        .await?;
    let expires_at = DateTime::from_timestamp(claims.expires_at, 0).unwrap_or(now + TTL);
    // WHY: ON CONFLICT DO NOTHING, not a caught unique violation: on Postgres that would abort a
    // surrounding transaction.
    let inserted = token_use::Entity::insert(token_use::ActiveModel {
        nonce_digest: Set(digest),
        expires_at: Set(expires_at),
    })
    .on_conflict(
        OnConflict::column(token_use::Column::NonceDigest)
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    // SECURITY: single use, on every node (test: a_socket_token_works_once).
    if inserted == 0 {
        return Err(TokenError::Replayed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Vec<u8> {
        "k".repeat(MIN_SECRET).into_bytes()
    }

    #[test]
    fn a_minted_token_verifies_until_it_expires() {
        let now = Utc::now();
        let token = mint(&secret(), 7, 42, now);
        let claims = verify(&secret(), &token, now).unwrap();
        assert_eq!((claims.person_id, claims.session_id), (7, 42));
        assert_eq!(
            verify(&secret(), &token, now + TTL),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn tokens_signed_with_another_secret_or_altered_are_refused() {
        let now = Utc::now();
        let token = mint(&secret(), 7, 42, now);
        let other = "o".repeat(MIN_SECRET).into_bytes();
        assert_eq!(verify(&other, &token, now), Err(TokenError::Signature));
        let altered = token.replacen("v1.7.", "v1.8.", 1);
        assert_eq!(verify(&secret(), &altered, now), Err(TokenError::Signature));
        assert_eq!(verify(&secret(), "nope", now), Err(TokenError::Malformed));
        assert_eq!(
            verify(&secret(), "v2.1.1.1.n.sig", now),
            Err(TokenError::Malformed)
        );
    }

    #[test]
    fn a_far_future_expiry_is_refused_even_when_signed() {
        let now = Utc::now();
        let token = mint(&secret(), 7, 42, now + Duration::hours(2));
        assert_eq!(verify(&secret(), &token, now), Err(TokenError::Expired));
    }
}
