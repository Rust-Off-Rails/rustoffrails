//! Who may open `/admin/jobs`. Applied as a `route_layer` on the whole dashboard router, so no
//! handler can forget it.
//! GUIDE: .offrails/guides/jobs-dashboard.md#mounting-and-access
//!
//! | Situation                                 | Rule                                            |
//! |-------------------------------------------|-------------------------------------------------|
//! | `APP_ENV=development`                     | open                                            |
//! | the app has accounts (`admin_from_session`) | admins only; others get 404, anonymous 401/redirect |
//! | `APP_ADMIN_PASSWORD` set                  | HTTP Basic, constant-time, failed tries limited |
//! | production with neither                   | not mounted at all (404)                        |

use std::time::Duration;

use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::{config::Config, controllers::application::ClientIp, error::AppError, state::AppState};

/// Failed passwords allowed per client IP per window.
const FAILED_LIMIT: u32 = 10;
const FAILED_WINDOW: Duration = Duration::from_secs(180);

/// Proof that the request may use the jobs dashboard.
pub struct AdminAccess;

/// Whether the dashboard is mounted at all.
/// SECURITY: in production without an admin gate the routes don't exist, so a misconfigured
/// deploy shows 404, never an open dashboard (test: production_without_an_admin_gate_is_404).
pub fn dashboard_enabled(config: &Config) -> bool {
    config.is_development() || config.jobs_dashboard.admin_password.is_some() || HAS_ACCOUNTS
}

/// APP: set to `true` once an authentication sample is installed and `admin_from_session`
/// below asks it; the dashboard is then mounted in production for admins.
pub const HAS_ACCOUNTS: bool = false;

/// APP: the hook into your authentication sample. Return `Some(true)` for a signed-in admin,
/// `Some(false)` for a signed-in non-admin, `None` when nobody is signed in (or the app has no
/// accounts). With the auth-password or auth-magic-link sample it looks like:
///
/// ```ignore
/// match crate::controllers::application::CurrentUser::from_request_parts(parts, state).await {
///     Ok(CurrentUser(user)) => Some(user.is_admin()),
///     Err(_) => None,
/// }
/// ```
///
/// Keep the extractor the app already uses for its own admin pages, so the session checks
/// (expiry, revocation, 2FA step-up) are the same ones.
async fn admin_from_session(_parts: &mut Parts, _state: &AppState) -> Option<bool> {
    None
}

impl FromRequestParts<AppState> for AdminAccess {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        if state.config.is_development() {
            return Ok(Self);
        }
        if let Some(verdict) = session_verdict(admin_from_session(parts, state).await) {
            return verdict.map(|()| Self);
        }
        let Some(expected) = state.config.jobs_dashboard.admin_password.as_deref() else {
            // Accounts but nobody signed in: the login page (an auth sample routes it).
            return Err(AppError::Unauthorized.into_response());
        };
        let ip = ClientIp::from_request_parts(parts, state)
            .await
            .unwrap_or(ClientIp(None));
        let key = ip.key("admin-basic-auth");
        // SECURITY: once a client has used up its failed tries, even the right password is
        // refused until the window passes, so guessing can't continue in the background
        // (test: failed_admin_passwords_are_rate_limited).
        if state.rate_limit.exhausted(&key, FAILED_LIMIT).await {
            return Err(AppError::TooManyRequests.into_response());
        }
        match basic_password(parts) {
            // The browser's first request carries no credentials: ask, don't count.
            None => Err(challenge()),
            Some(given) if same_secret(&given, expected) => Ok(Self),
            Some(_) => {
                if state
                    .rate_limit
                    .check(&key, FAILED_LIMIT, FAILED_WINDOW)
                    .await
                    .is_err()
                {
                    return Err(AppError::TooManyRequests.into_response());
                }
                tracing::warn!(client = %key, "jobs dashboard: wrong admin password");
                Err(challenge())
            }
        }
    }
}

/// What a signed-in user's role decides: admins in, everyone else a 404; `None` (nobody signed
/// in) falls through to the password, or the login page.
/// SECURITY: a signed-in non-admin learns nothing: the dashboard "doesn't exist"
/// (test: a_signed_in_non_admin_gets_a_404).
fn session_verdict(is_admin: Option<bool>) -> Option<Result<(), Response>> {
    match is_admin {
        Some(true) => Some(Ok(())),
        Some(false) => Some(Err(AppError::NotFound.into_response())),
        None => None,
    }
}

/// 401 with `WWW-Authenticate: Basic realm="admin"`.
fn challenge() -> Response {
    let mut res = (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    res.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"admin\", charset=\"UTF-8\""),
    );
    res
}

/// The password of an `Authorization: Basic …` header (any user name).
fn basic_password(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (_, password) = text.split_once(':')?;
    Some(password.to_owned())
}

/// SECURITY: compares SHA-256 digests in constant time, so response timing doesn't reveal how
/// much of a guess was right, or the password's length (test: secrets_compare_exactly).
fn same_secret(given: &str, expected: &str) -> bool {
    let a = Sha256::digest(given.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn parts(auth: Option<&str>) -> Parts {
        let mut req = Request::builder().uri("/admin/jobs");
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        req.body(()).unwrap().into_parts().0
    }

    #[test]
    fn basic_credentials_parse() {
        let b64 = base64::engine::general_purpose::STANDARD.encode("admin:s3cret:with:colons");
        assert_eq!(
            basic_password(&parts(Some(&format!("Basic {b64}")))).as_deref(),
            Some("s3cret:with:colons")
        );
        assert_eq!(basic_password(&parts(None)), None);
        assert_eq!(basic_password(&parts(Some("Bearer x"))), None);
        assert_eq!(basic_password(&parts(Some("Basic !!!"))), None);
    }

    #[test]
    fn a_signed_in_non_admin_gets_a_404() {
        assert!(matches!(session_verdict(Some(true)), Some(Ok(()))));
        let denied = session_verdict(Some(false)).unwrap().unwrap_err();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        assert!(session_verdict(None).is_none());
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(same_secret("correct horse", "correct horse"));
        assert!(!same_secret("correct hors", "correct horse"));
        assert!(!same_secret("", "x"));
    }
}
