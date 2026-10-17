//! Encryption at rest for TOTP secrets: AES-256-GCM with a key from config
//! (`APP_TOTP_ENCRYPTION_KEY`), a random nonce per value, and the owner's id as associated data.
//!
//! Stored format: `v1:<base64url(nonce || ciphertext+tag)>`.
//!
//! - A database leak alone reveals no TOTP secret (the key isn't in the database).
//! - The user id is authenticated with the ciphertext, so a row copied onto another account
//!   fails to decrypt instead of giving that account a known secret.
//! - `APP_TOTP_ENCRYPTION_KEY_PREVIOUS` keeps old values readable during a key rotation; they are
//!   re-encrypted under the new key the next time they're used.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

const PREFIX: &str = "v1:";

/// Encrypts `plain` for `owner_id`.
// SECURITY: a fresh random 96-bit nonce per encryption; reusing a GCM nonce under one key
// would leak the authentication key (test: encrypted_secrets_round_trip_and_bind_to_their_owner)
pub fn seal(key: &[u8; 32], owner_id: i64, plain: &[u8]) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("bad key length"))?;
    let mut nonce_bytes = [0u8; 12];
    rand::fill(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);
    let aad = owner_id.to_be_bytes();
    let sealed = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plain,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&sealed);
    Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(out)))
}

/// Decrypts a value sealed for `owner_id` with any of `keys` (current first). `None` when no key
/// opens it (tampered, wrong owner, or an unknown key).
pub fn open(keys: &[&[u8; 32]], owner_id: i64, sealed: &str) -> Option<(Vec<u8>, usize)> {
    let raw = URL_SAFE_NO_PAD.decode(sealed.strip_prefix(PREFIX)?).ok()?;
    if raw.len() < 12 + 16 {
        return None;
    }
    let (nonce_bytes, body) = raw.split_at(12);
    let nonce_arr: [u8; 12] = nonce_bytes.try_into().ok()?;
    let nonce = Nonce::from(nonce_arr);
    let aad = owner_id.to_be_bytes();
    keys.iter().enumerate().find_map(|(i, key)| {
        let cipher = Aes256Gcm::new_from_slice(*key).ok()?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: &aad,
                },
            )
            .ok()
            .map(|plain| (plain, i))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_secrets_round_trip_and_bind_to_their_owner() {
        let mut key = [0u8; 32];
        rand::fill(&mut key);
        let mut other = [0u8; 32];
        rand::fill(&mut other);
        let a = seal(&key, 7, b"twenty byte secret!!").unwrap();
        let b = seal(&key, 7, b"twenty byte secret!!").unwrap();
        assert_ne!(a, b, "nonces must differ");
        assert!(!a.contains("twenty"));
        assert_eq!(open(&[&key], 7, &a).unwrap().0, b"twenty byte secret!!");
        assert!(
            open(&[&key], 8, &a).is_none(),
            "another owner can't open it"
        );
        assert!(
            open(&[&other], 7, &a).is_none(),
            "another key can't open it"
        );
        assert_eq!(
            open(&[&other, &key], 7, &a).unwrap().1,
            1,
            "the previous key still opens it"
        );
        let mut tampered = a.clone();
        tampered.replace_range(10..11, if &a[10..11] == "A" { "B" } else { "A" });
        assert!(open(&[&key], 7, &tampered).is_none());
        assert!(open(&[&key], 7, "garbage").is_none());
    }
}
