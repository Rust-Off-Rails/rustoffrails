//! Minimal cookie helpers: read from a `Cookie` header, build `Set-Cookie` values.
//! Values written here are always percent-encoded or base64url, so they are header-safe.

use axum::http::{HeaderMap, HeaderValue, header};

/// Reads a cookie's raw (still encoded) value from the request headers.
pub fn get(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else { continue };
        for pair in value.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=')
                && k == name
            {
                return Some(v.to_owned());
            }
        }
    }
    None
}

/// Builds a `Set-Cookie` header value. `max_age_secs: None` makes it a session cookie.
pub fn set(name: &str, value: &str, max_age_secs: Option<i64>, secure: bool) -> HeaderValue {
    let mut s = format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax");
    if let Some(age) = max_age_secs {
        s.push_str(&format!("; Max-Age={age}"));
    }
    if secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Builds a removal `Set-Cookie` header value.
pub fn clear(name: &str, secure: bool) -> HeaderValue {
    set(name, "", Some(0), secure)
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inverse of [`encode`]. Returns `None` for malformed escapes or invalid UTF-8.
pub fn decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = input.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_json_and_unicode() {
        let s = r#"{"kind":"notice","message":"Saved — ¡listo!"}"#;
        assert_eq!(decode(&encode(s)).as_deref(), Some(s));
    }

    #[test]
    fn rejects_malformed_escape() {
        assert_eq!(decode("%zz"), None);
        assert_eq!(decode("%4"), None);
    }

    #[test]
    fn reads_cookie_from_header() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; session_token=abc; b=2"),
        );
        assert_eq!(get(&h, "session_token").as_deref(), Some("abc"));
        assert_eq!(get(&h, "missing"), None);
    }
}
