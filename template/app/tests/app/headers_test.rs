//! The header matrix per response class, in development and in production (guides/security.md).
//! AC-security-1 .. see product/features/*.md `## Security`.

use axum::http::{HeaderMap, StatusCode, header};
use offrails_app::config::{AppEnv, Config};
use offrails_app::kernel::security::DEV_CSP_EXTRA;

use crate::support::TestApp;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   font-src 'self'; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; \
                   base-uri 'self'; form-action 'self'";

fn development() -> Config {
    Config {
        env: AppEnv::Development,
        ..Config::test()
    }
}

/// Production with an https `APP_URL`, as deployed behind TLS.
fn production() -> Config {
    Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    }
}

fn value(h: &HeaderMap, name: &str) -> String {
    h.get(name)
        .unwrap_or_else(|| panic!("missing header {name}: {h:?}"))
        .to_str()
        .unwrap()
        .to_owned()
}

/// The full set from the matrix. `tls` is true for production with an https `APP_URL`.
fn assert_matrix(what: &str, h: &HeaderMap, tls: bool) {
    let expected_csp = if tls {
        format!("{CSP}; upgrade-insecure-requests")
    } else {
        CSP.to_owned()
    };
    let csp = value(h, "content-security-policy");
    // Development may add the directives samples declare (never production: below).
    let dev_extra: String = DEV_CSP_EXTRA.iter().map(|d| format!("; {d}")).collect();
    let csp = csp.strip_suffix(dev_extra.as_str()).unwrap_or(&csp);
    assert_eq!(csp, expected_csp, "{what}");
    assert_eq!(value(h, "x-content-type-options"), "nosniff", "{what}");
    assert_eq!(
        value(h, "referrer-policy"),
        "strict-origin-when-cross-origin",
        "{what}"
    );
    assert_eq!(value(h, "x-frame-options"), "DENY", "{what}");
    assert_eq!(
        value(h, "cross-origin-opener-policy"),
        "same-origin",
        "{what}"
    );
    assert_eq!(
        value(h, "cross-origin-resource-policy"),
        "same-origin",
        "{what}"
    );
    let pp = value(h, "permissions-policy");
    for feature in ["camera=()", "microphone=()", "geolocation=()", "payment=()"] {
        assert!(pp.contains(feature), "{what}: {pp}");
    }
    assert!(h.get("cross-origin-embedder-policy").is_none(), "{what}");
    let hsts = value(h, "strict-transport-security");
    if tls {
        assert_eq!(hsts, "max-age=63072000; includeSubDomains", "{what}");
    } else {
        assert_eq!(hsts, "max-age=0", "{what}");
    }
}

fn assert_no_store(what: &str, h: &HeaderMap) {
    assert_eq!(value(h, "cache-control"), "no-store", "{what}");
}

async fn matrix_for(config: Config, tls: bool) {
    let development = config.is_development();
    let app = TestApp::with_config(config).await;

    // HTML page, anonymous
    let res = app.get("/").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_matrix("html page", &res.headers, tls);
    assert!(
        res.headers
            .get(header::CACHE_CONTROL)
            .is_none_or(|v| v != "no-store"),
        "anonymous pages may be cached by the browser"
    );

    // 404
    let res = app.get("/no/such/page").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_matrix("404", &res.headers, tls);

    // 403 (CSRF)
    let res = app
        .post_form_with_headers("/", &[], &[("sec-fetch-site", "cross-site")])
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_matrix("403 csrf", &res.headers, tls);

    // JSON error
    let res = app.get("/api/nope").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_matrix("api 404", &res.headers, tls);

    // Static asset: full matrix, cacheable
    let res = app.get("/static/favicon.svg").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_matrix("static asset", &res.headers, tls);
    assert_ne!(value(&res.headers, "cache-control"), "no-store");

    // A request with a session cookie is never stored; static files stay cacheable.
    let session = if tls {
        "__Host-session_token"
    } else {
        "session_token"
    };
    app.set_cookie(session, "x");
    let res = app.get("/").await;
    assert_no_store("page with a session cookie", &res.headers);
    let res = app.get("/static/favicon.svg").await;
    assert_ne!(value(&res.headers, "cache-control"), "no-store");
    app.clear_cookies();

    // Kernel page and redirect (development only: production doesn't route /dev)
    if development {
        let res = app.get("/dev").await;
        assert_eq!(res.status, StatusCode::OK);
        assert_matrix("kernel page", &res.headers, tls);
        assert_no_store("kernel page", &res.headers);

        let res = app
            .post_form("/dev/migrations", &[("return_to", "/")])
            .await;
        assert!(res.status.is_redirection(), "{:?}", res.status);
        assert_matrix("redirect", &res.headers, tls);
        assert_no_store("kernel redirect", &res.headers);
    }
}

#[tokio::test]
async fn every_response_class_carries_the_header_matrix_in_development() {
    matrix_for(development(), false).await;
}

#[tokio::test]
async fn every_response_class_carries_the_header_matrix_in_production() {
    matrix_for(production(), true).await;
}

#[tokio::test]
async fn production_over_plain_http_sends_no_tls_only_directives() {
    let app = TestApp::with_config(Config {
        app_url: "http://localhost:3000".into(),
        ..production()
    })
    .await;
    let res = app.get("/").await;
    assert_matrix("http production", &res.headers, false);
    assert!(!value(&res.headers, "content-security-policy").contains("upgrade-insecure-requests"));
}

#[tokio::test]
async fn request_head_returns_headers_without_reading_the_body() {
    let app = TestApp::with_config(production()).await;
    let (status, headers) = app.request_head("/", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_matrix("head", &headers, true);
}

/// Development-only CSP directives (a report endpoint) never reach production.
#[tokio::test]
async fn production_csp_has_no_development_directives() {
    let app = TestApp::with_config(production()).await;
    let res = app.get("/").await;
    let csp = value(&res.headers, "content-security-policy");
    for directive in DEV_CSP_EXTRA {
        assert!(!csp.contains(directive), "{csp}");
    }
    assert!(!csp.contains("report-uri"), "{csp}");
}
