//! "Take me back where I was going" after sign-in.
//!
//! GUIDE: .offrails/guides/auth.md#return-to-kernelreturn_tors
//!
//! The path is stored base64url-encoded (paths contain `;` and `,`) in a short-lived cookie when
//! a signed-out visitor hits a protected GET, and read once after sign-in.

use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::session as auth_cookie;

const BASE: &str = "return_to";
// APP: how long a visitor has to sign in before the destination is forgotten.
const MAX_AGE_SECS: i64 = 15 * 60;

/// Only a same-origin path: starts with one `/`, not `//` or `/\`, no control characters, no
/// backslash anywhere, bounded length.
// SECURITY: `//evil.example` and `/\evil.example` are protocol-relative URLs to browsers, so
// accepting them turns sign-in into an open redirect (phishing that starts on our domain).
// INVARIANT: return_to only ever redirects to a local path
// (test: return_to_rejects_open_redirects)
pub fn is_safe(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.len() <= 2000
}

/// Remembers `path` (ignored when unsafe).
pub fn set(path: &str, secure: bool) -> Option<HeaderValue> {
    is_safe(path).then(|| {
        auth_cookie::set(
            &auth_cookie::name(BASE, secure),
            &URL_SAFE_NO_PAD.encode(path.as_bytes()),
            Some(MAX_AGE_SECS),
            secure,
        )
    })
}

pub fn clear(secure: bool) -> HeaderValue {
    auth_cookie::clear(&auth_cookie::name(BASE, secure), secure)
}

/// The remembered path, if present and safe.
pub fn read(headers: &HeaderMap, secure: bool) -> Option<String> {
    auth_cookie::read(headers, &auth_cookie::name(BASE, secure))
        .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|p| is_safe(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_paths_are_safe() {
        assert!(is_safe("/account?tab=sessions"));
        for bad in [
            "//evil.example",
            "/\\evil.example",
            "/a\\b",
            "https://evil.example",
            "evil.example",
            "/a\nb",
            "/a\tb",
            "",
        ] {
            assert!(!is_safe(bad), "{bad:?} was accepted");
        }
        assert!(!is_safe(&format!("/{}", "a".repeat(2000))));
    }

    #[test]
    fn round_trips_paths_with_separators() {
        let set = set("/boards/1?a=b;c,d", false).unwrap();
        let pair = set.to_str().unwrap().split(';').next().unwrap().to_owned();
        let mut h = HeaderMap::new();
        h.insert("cookie", HeaderValue::from_str(&pair).unwrap());
        assert_eq!(read(&h, false).as_deref(), Some("/boards/1?a=b;c,d"));
        assert!(super::set("//evil.example", false).is_none());
    }

    #[test]
    fn a_tampered_cookie_is_ignored() {
        let mut h = HeaderMap::new();
        let v = format!("return_to={}", URL_SAFE_NO_PAD.encode("//evil.example"));
        h.insert("cookie", HeaderValue::from_str(&v).unwrap());
        assert_eq!(read(&h, false), None);
    }
}
