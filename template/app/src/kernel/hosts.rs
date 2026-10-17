//! Development-only Host authorization (Rails' `HostAuthorization`): a request whose `Host` isn't
//! this machine, `APP_URL`'s host or an `APP_ALLOWED_HOSTS` entry gets a 403 page that says how to
//! allow it.
//!
//! Why: development serves /dev tools without a login (mailbox, dashboard, SQL, logs), trusting
//! that only this machine can reach them. DNS rebinding breaks that trust: a page on
//! `evil.example` re-points its own name at 127.0.0.1, and the browser then sends
//! `Host: evil.example` to this server *same-origin*, so neither the CORS rules nor the CSRF
//! layer (Fetch Metadata says `same-origin`) stop it from reading /dev/mailbox. The `Host` header
//! is the one thing the attacker can't set to `localhost`.
//!
//! Production is not checked here: the platform's proxy routes by host, and an app answering any
//! `Host` there exposes nothing that isn't public.
//! GUIDE: .offrails/guides/security.md

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::config::Config;

/// The hosts development answers. `None` outside development (no check).
#[derive(Clone, Debug)]
pub struct AllowedHosts(Option<Vec<String>>);

impl AllowedHosts {
    pub fn new(config: &Config) -> Self {
        if !config.is_development() {
            return Self(None);
        }
        // APP: names development always answers; add yours with APP_ALLOWED_HOSTS, not here.
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .iter()
            .map(|h| (*h).to_owned())
            .collect();
        if let Some(h) = host_of_url(&config.app_url) {
            hosts.push(h);
        }
        hosts.extend(config.allowed_hosts.iter().map(|h| normalize(h)));
        Self(Some(hosts))
    }

    /// `Host` values this allows (`host` or `host:port`, any case). A missing `Host` (an
    /// HTTP/1.0 client, a test request) is allowed: a browser always sends one, and a rebinding
    /// attack needs a browser.
    pub fn allows(&self, host: Option<&str>) -> bool {
        let Some(list) = &self.0 else {
            return true;
        };
        let Some(host) = host else {
            return true;
        };
        let host = normalize(host);
        // `*.localhost` resolves to this machine by definition (RFC 6761).
        host.ends_with(".localhost")
            || list.iter().any(|allowed| {
                allowed == &host
                    || allowed
                        .strip_prefix('.')
                        .is_some_and(|domain| host == domain || host.ends_with(allowed.as_str()))
            })
    }
}

/// `Host: Dev.Example.com:3000` → `dev.example.com`; `[::1]:3000` → `::1`.
fn normalize(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default().to_owned();
    }
    if host.matches(':').count() > 1 {
        return host; // a bare IPv6 address has no port
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name.to_owned(),
        _ => host,
    }
}

fn host_of_url(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = normalize(authority);
    (!host.is_empty()).then_some(host)
}

/// The request's host: the URI authority (HTTP/2's `:authority`), else the `Host` header.
fn request_host(req: &Request) -> Option<String> {
    if let Some(authority) = req.uri().authority() {
        return Some(authority.as_str().to_owned());
    }
    req.headers()
        .get(header::HOST)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

// SECURITY: in development, a request for a Host that isn't this machine (DNS rebinding, or a
// LAN visitor when APP_BIND opens the port) is refused before any handler or /dev tool runs
// (test: development_refuses_a_rebound_host_name).
pub async fn middleware(State(hosts): State<AllowedHosts>, req: Request, next: Next) -> Response {
    let host = request_host(&req);
    if hosts.allows(host.as_deref()) {
        return next.run(req).await;
    }
    let shown = normalize(host.as_deref().unwrap_or_default());
    tracing::warn!(host = %shown, "blocked a request for a host development doesn't serve (APP_ALLOWED_HOSTS)");
    refused(&shown)
}

fn refused(host: &str) -> Response {
    let host = escape(host);
    let body = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Blocked host</title></head>\n<body><main>\n<h1>Blocked host: {host}</h1>\n<p>In development this app answers only requests addressed to this machine (localhost, 127.0.0.1, [::1]) or to the host in <code>APP_URL</code>. That stops a web page from re-pointing its own domain at your machine (DNS rebinding) and reading your development tools.</p>\n<p>If you meant to open it through <strong>{host}</strong> (a tunnel or a name on your network), allow it and restart:</p>\n<pre>APP_ALLOWED_HOSTS={host}</pre>\n<p>A leading dot allows every subdomain (<code>APP_ALLOWED_HOSTS=.{host}</code>). To reach the app from another device at all, it must also listen beyond this machine: <code>APP_BIND=0.0.0.0</code>.</p>\n</main></body></html>\n"
    );
    let mut response = (StatusCode::FORBIDDEN, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '&' => "&amp;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppEnv;

    fn dev(app_url: &str, allowed: &[&str]) -> AllowedHosts {
        AllowedHosts::new(&Config {
            env: AppEnv::Development,
            app_url: app_url.into(),
            allowed_hosts: allowed.iter().map(|h| (*h).to_owned()).collect(),
            ..Config::test()
        })
    }

    #[test]
    fn hosts_match_by_name_without_port_or_case() {
        let hosts = dev(
            "https://dev.example.com",
            &["tunnel.example", ".lan.example"],
        );
        for ok in [
            "localhost:3000",
            "LOCALHOST",
            "127.0.0.1:3000",
            "[::1]:3000",
            "app.localhost:3000",
            "dev.example.com",
            "tunnel.example:443",
            "lan.example",
            "box.lan.example:3000",
        ] {
            assert!(hosts.allows(Some(ok)), "{ok} should be allowed");
        }
        for refused in [
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "evillan.example",
            "tunnel.example.evil.example",
            "",
        ] {
            assert!(!hosts.allows(Some(refused)), "{refused} should be refused");
        }
        assert!(hosts.allows(None));
    }

    #[test]
    fn the_refusal_page_escapes_the_host() {
        let html = escape("<script>");
        assert_eq!(html, "&lt;script&gt;");
    }
}
