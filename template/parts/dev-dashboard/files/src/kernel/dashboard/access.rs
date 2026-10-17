//! Who may open the read-only dashboard at `/admin/dashboard`. Applied as a `route_layer` on that
//! whole router, so no handler can forget it. `/dev/dashboard` (the full one) is development only.
//!
//! | Situation | `/admin/dashboard` |
//! |-----------|--------------------|
//! | development | open (it previews what production shows) |
//! | production, `DASHBOARD_IN_PRODUCTION=true`, the app has admin accounts | admins only; others 404, anonymous to login |
//! | production, `DASHBOARD_IN_PRODUCTION=true`, `APP_ADMIN_PASSWORD` set | HTTP Basic, constant time, failed tries limited |
//! | anything else | not mounted (404), and nothing is recorded |

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::{HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::controllers::application::ClientIp;
use crate::error::AppError;
use crate::state::AppState;

const FAILED_LIMIT: u32 = 10;
const FAILED_WINDOW: Duration = Duration::from_secs(180);

// Who is signed in, when the app has accounts. With auth-core installed, an optional wire puts an
// explicit `pub use` of `accounts::{HAS_ACCOUNTS, admin_from_session}` here; explicit names win
// over the glob below, so `no_accounts` stays compiled but unused.
// samples: dashboard accounts
#[allow(unused_imports)] // WHY: with auth-core, both names come from `accounts` instead
pub use no_accounts::*;

/// Without an authentication sample: no accounts, so only `APP_ADMIN_PASSWORD` opens the view.
pub mod no_accounts {
    use axum::http::request::Parts;

    use crate::state::AppState;

    pub const HAS_ACCOUNTS: bool = false;

    /// `Some(true)` for a signed-in admin, `Some(false)` for a signed-in non-admin, `None` when
    /// nobody is signed in. Nobody ever is, here.
    pub async fn admin_from_session(_parts: &mut Parts, _state: &AppState) -> Option<bool> {
        None
    }
}

/// Warned once per process: X-Forwarded-For arrived but `APP_TRUSTED_PROXY` is off.
static UNTRUSTED_FORWARDED_WARNED: AtomicBool = AtomicBool::new(false);

/// Behind a proxy without `APP_TRUSTED_PROXY`, every client has the proxy's address, so they all
/// share one failed-password budget: anyone can lock the admins out (for `FAILED_WINDOW` at a
/// time) by failing on purpose. A known trade-off of keying by IP; this makes it visible.
// SECURITY: the misconfiguration that turns the lockout into a denial of service is logged once
// (test: an_untrusted_forwarded_for_header_warns_once)
fn warn_if_forwarded_untrusted(parts: &Parts, state: &AppState) {
    if !state.config.trusted_proxy
        && parts.headers.contains_key("x-forwarded-for")
        && !UNTRUSTED_FORWARDED_WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            "dashboard: requests carry X-Forwarded-For but APP_TRUSTED_PROXY is off, so every client \
             behind the proxy shares one failed-password budget and anyone can lock the admins out; \
             set APP_TRUSTED_PROXY=true when the app runs behind a proxy"
        );
    }
}

/// Whether `/admin/dashboard` is mounted in production.
// SECURITY: off unless asked for, and never without a gate: a misconfigured deploy shows 404,
// not an open dashboard (test: the_dashboard_is_404_in_production_by_default).
pub fn mounted_in_production(config: &Config) -> bool {
    config.is_production()
        && config.dashboard.in_production
        && (config.dashboard.admin_password.is_some() || HAS_ACCOUNTS)
}

/// Whether `/admin/dashboard` is mounted in this environment.
pub fn admin_mounted(config: &Config) -> bool {
    config.is_development() || mounted_in_production(config)
}

/// Proof the request may read the dashboard.
pub struct DashboardAccess;

impl FromRequestParts<AppState> for DashboardAccess {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        if state.config.is_development() {
            return Ok(Self);
        }
        match admin_from_session(parts, state).await {
            Some(true) => return Ok(Self),
            // SECURITY: a signed-in non-admin learns nothing: the page "doesn't exist", even with
            // the right Basic password (test: a_signed_in_non_admin_gets_404_from_the_production_view).
            Some(false) => return Err(AppError::NotFound.into_response()),
            None => {}
        }
        let Some(expected) = state.config.dashboard.admin_password.as_deref() else {
            return Err(AppError::Unauthorized.into_response());
        };
        warn_if_forwarded_untrusted(parts, state);
        let ip = ClientIp::from_request_parts(parts, state)
            .await
            .unwrap_or(ClientIp(None));
        let key = ip.key("dashboard-basic-auth");
        // SECURITY: once a client used up its failed tries, even the right password waits for the
        // window (test: the_production_dashboard_needs_the_admin_password).
        if state.rate_limit.exhausted(&key, FAILED_LIMIT).await {
            return Err(AppError::TooManyRequests.into_response());
        }
        match basic_password(parts) {
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
                tracing::warn!(client = %key, "dashboard: wrong admin password");
                Err(challenge())
            }
        }
    }
}

fn challenge() -> Response {
    let mut res = (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    res.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"admin\", charset=\"UTF-8\""),
    );
    res
}

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

/// Compares the SHA-256 digests of both (fixed-length, so the length of the password leaks
/// nothing) with a branch-free fold over every byte: no early exit that would hint how much of a
/// guess was right. Best effort: the compiler may still optimize, which is why the rate limit, not
/// this, is the real guard.
pub fn same_secret(given: &str, expected: &str) -> bool {
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

    #[test]
    fn dashboard_secrets_compare_exactly() {
        assert!(same_secret(
            "correct horse battery",
            "correct horse battery"
        ));
        assert!(!same_secret(
            "correct horse batter",
            "correct horse battery"
        ));
    }
}
