//! The cookies auth sets: names, flags and reading them back. Every auth cookie goes through
//! here, so the flags can't drift between features.
//!
//! GUIDE: .offrails/guides/auth.md#sessions
//! GUIDE: .offrails/guides/security.md#the-header-matrix (Cookies)

use axum::http::{HeaderMap, HeaderValue};

use super::cookie;
use super::security::{SESSION_COOKIE_DEV, SESSION_COOKIE_PROD};

/// A session lasts 30 days from the last request (sliding), and never longer than 90 days from
/// sign-in (absolute), whatever the activity.
// APP: change both to taste (a bank might use 1 day idle / 7 days absolute). Keep idle <= absolute.
pub const IDLE_DAYS: i64 = 30;
// APP: see IDLE_DAYS.
pub const ABSOLUTE_DAYS: i64 = 90;

/// The session cookie's name: `__Host-session_token` in production.
// SECURITY: `__Host-` makes the browser refuse the cookie unless it is Secure, Path=/ and has
// no Domain, so a sibling subdomain can't plant a session (fixation, login CSRF)
// (test: production_auth_cookies_are_host_prefixed_and_secure)
pub fn session_name(secure: bool) -> &'static str {
    if secure {
        SESSION_COOKIE_PROD
    } else {
        SESSION_COOKIE_DEV
    }
}

/// Any other auth cookie (`device`, `pending_login`, …), with the same `__Host-` rule.
pub fn name(base: &'static str, secure: bool) -> String {
    if secure {
        format!("__Host-{base}")
    } else {
        base.to_owned()
    }
}

/// `Set-Cookie` for an auth cookie: always `Path=/; HttpOnly; SameSite=Lax`, plus `Secure` in
/// production.
// SECURITY: HttpOnly keeps tokens away from injected scripts; SameSite=Lax stops cross-site
// POSTs from carrying the cookie; Secure keeps it off plain http.
// INVARIANT: every auth cookie is HttpOnly + SameSite=Lax, and Secure + __Host- in production
// (test: production_auth_cookies_are_host_prefixed_and_secure)
pub fn set(name: &str, value: &str, max_age_secs: Option<i64>, secure: bool) -> HeaderValue {
    cookie::set(name, value, max_age_secs, secure)
}

pub fn clear(name: &str, secure: bool) -> HeaderValue {
    cookie::clear(name, secure)
}

/// The session cookie, set for the session's full absolute lifetime.
pub fn set_session(token: &str, secure: bool) -> HeaderValue {
    set(
        session_name(secure),
        token,
        Some(ABSOLUTE_DAYS * 24 * 60 * 60),
        secure,
    )
}

pub fn clear_session(secure: bool) -> HeaderValue {
    clear(session_name(secure), secure)
}

/// Reads a cookie by its exact name. Production reads only the `__Host-` name, so a plain
/// `session_token` planted over http (or by a subdomain) is ignored.
// SECURITY: accepting the unprefixed name in production would reopen session fixation.
// INVARIANT: production ignores an unprefixed session cookie
// (test: production_ignores_a_planted_unprefixed_session_cookie)
pub fn read(headers: &HeaderMap, name: &str) -> Option<String> {
    cookie::get(headers, name).filter(|v| !v.is_empty() && v.len() <= 512)
}

pub fn read_session(headers: &HeaderMap, secure: bool) -> Option<String> {
    read(headers, session_name(secure))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_names_are_host_prefixed() {
        assert_eq!(session_name(true), "__Host-session_token");
        assert_eq!(session_name(false), "session_token");
        assert_eq!(name("device", true), "__Host-device");
        assert_eq!(name("device", false), "device");
    }

    #[test]
    fn flags() {
        let dev = set_session("t", false);
        let dev = dev.to_str().unwrap();
        assert!(dev.contains("HttpOnly") && dev.contains("SameSite=Lax") && dev.contains("Path=/"));
        assert!(!dev.contains("Secure") && !dev.contains("Domain"));
        let prod = set_session("t", true);
        let prod = prod.to_str().unwrap();
        assert!(prod.starts_with("__Host-session_token=t") && prod.contains("Secure"));
    }

    #[test]
    fn production_ignores_the_plain_name() {
        let mut h = HeaderMap::new();
        h.insert("cookie", HeaderValue::from_static("session_token=abc"));
        assert_eq!(read_session(&h, false).as_deref(), Some("abc"));
        assert_eq!(read_session(&h, true), None);
    }
}
