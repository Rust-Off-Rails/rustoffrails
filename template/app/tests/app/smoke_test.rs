//! The skeleton's smoke tests (generators/new.md §7) plus framework-level guarantees.

use axum::http::StatusCode;
use base64::Engine;
use offrails_app::{
    config::{AppEnv, Config},
    error::ROUTE_FALLBACK,
    routes::{Access, ROUTES},
};
use sea_orm::ConnectionTrait;

use crate::support::TestApp;

#[tokio::test]
async fn home_page_returns_200_and_renders_the_layout() {
    let app = TestApp::new().await;
    app.get("/")
        .await
        .assert_ok()
        .assert_contains("<title>Offrails App: A new Rust off Rails app</title>")
        .assert_contains("/static/app.css")
        .assert_contains(r#"rel="icon""#);
}

#[tokio::test]
async fn up_returns_200() {
    let app = TestApp::new().await;
    app.get("/up").await.assert_ok().assert_contains("ok");
}

#[tokio::test]
async fn an_unknown_path_returns_the_404_page_marked_as_the_router_fallback() {
    let app = TestApp::new().await;
    let res = app.get("/definitely/not/here").await;
    res.assert_status(StatusCode::NOT_FOUND)
        .assert_contains("Page not found");
    assert_eq!(res.headers.get(&ROUTE_FALLBACK).unwrap(), "1");
}

#[tokio::test]
async fn an_unknown_api_path_is_json() {
    let app = TestApp::new().await;
    let res = app.get("/api/nope").await;
    res.assert_status(StatusCode::NOT_FOUND);
    insta::assert_snapshot!(res.body, @r#"{"error":"not_found"}"#);
}

#[tokio::test]
async fn error_pages_are_standalone_documents() {
    let app = TestApp::new().await;
    let page = app.get("/definitely/not/here").await;
    // No app layout: no nav, no htmx.
    page.assert_not_contains("<nav").assert_not_contains("htmx");
    page.assert_contains("/static/app.css")
        .assert_contains(r#"<meta name="color-scheme" content="light dark">"#);
}

#[tokio::test]
async fn security_headers_are_present() {
    let app = TestApp::new().await;
    let res = app.get("/").await;
    let h = &res.headers;
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        h.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("script-src 'self'"));
    assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"));
    assert!(h.get("x-request-id").is_some());
    // HSTS is `max-age=0` outside production (so a dev host is never pinned to https).
    assert_eq!(h.get("strict-transport-security").unwrap(), "max-age=0");
}

#[tokio::test]
async fn hsts_is_enabled_in_production() {
    let mut config = Config::test();
    config.env = AppEnv::Production;
    config.app_url = "https://app.example.com".into();
    let app = TestApp::with_config(config).await;
    let res = app.get("/").await;
    assert_eq!(
        res.headers.get("strict-transport-security").unwrap(),
        "max-age=63072000; includeSubDomains"
    );
}

#[tokio::test]
async fn sqlite_connections_enforce_foreign_keys() {
    let app = TestApp::new().await;
    if !app.on_sqlite() {
        return;
    }
    let row = app
        .db()
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "PRAGMA foreign_keys".to_owned(),
        ))
        .await
        .unwrap()
        .expect("pragma row");
    let enabled: i32 = row.try_get_by_index(0).unwrap();
    assert_eq!(enabled, 1);
}

#[tokio::test]
async fn a_flash_is_rendered_once_then_cleared() {
    let app = TestApp::new().await;
    let flash = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"kind":"notice","message":"Saved"}"#);
    // The cookie the kernel writes is percent-encoded JSON; set it the way the browser returns it.
    app.set_cookie(
        "_flash",
        &offrails_app::kernel::cookie::encode(r#"{"kind":"notice","message":"Saved"}"#),
    );
    let res = app.get("/").await;
    res.assert_ok().assert_contains("Saved");
    assert!(
        res.set_cookies()
            .iter()
            .any(|c| c.starts_with("_flash=;") && c.contains("Max-Age=0"))
    );
    app.get("/").await.assert_not_contains("Saved");
    // A cookie that isn't the kernel's format renders nothing and breaks nothing.
    app.set_cookie("_flash", &flash);
    app.get("/").await.assert_ok().assert_not_contains("Saved");
}

#[tokio::test]
async fn every_listed_route_is_routable_in_development() {
    let mut config = Config::test();
    config.env = AppEnv::Development;
    let app = TestApp::with_config(config).await;

    for (method, path, handler, _access) in ROUTES {
        let path = crate::security_test::concrete(path);
        // WHY: GETs read headers only: a streaming route (SSE) never ends its body.
        let (status, headers) = match *method {
            "GET" => app.request_head(&path, &[]).await,
            other => {
                let res = app.request_method(other, &path).await;
                (res.status, res.headers)
            }
        };
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} ({handler}) has no handler"
        );
        // A handler's own 404 is fine; the router fallback is "not routed".
        assert!(
            headers.get(&ROUTE_FALLBACK).is_none(),
            "{method} {path} ({handler}) is in ROUTES but not routed"
        );
    }
}

#[tokio::test]
async fn the_routes_table_is_the_routers_surface() {
    // Every dev route is listed as Dev, and nothing else is.
    for (_, path, _, access) in ROUTES {
        assert_eq!(path.starts_with("/dev"), *access == Access::Dev, "{path}");
    }
}

#[tokio::test]
async fn static_assets_are_cached_by_digest() {
    let app = TestApp::new().await;
    let page = app.get("/").await;
    let css = page
        .body
        .split("href=\"")
        .find(|s| s.starts_with("/static/app.css"))
        .and_then(|s| s.split('"').next())
        .expect("css link")
        .to_owned();
    if !std::path::Path::new("static/app.css").exists() {
        return; // built output (bin/setup builds it); the digest needs the file
    }
    assert!(css.contains("?v="), "{css}");

    let res = app.get(&css).await;
    res.assert_ok();
    assert_eq!(
        res.headers.get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
    let plain = app.get(css.split('?').next().unwrap()).await;
    assert_eq!(plain.headers.get("cache-control").unwrap(), "no-cache");
    // A missing file is never marked immutable.
    let missing = app.get("/static/nope.css?v=deadbeef").await;
    missing.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(missing.headers.get("cache-control").unwrap(), "no-cache");
}

#[tokio::test]
async fn the_home_page_query_count_is_stable() {
    let app = TestApp::new().await;
    app.assert_query_count_stable("/", async |_n| {}).await;
}

/// Every meta, favicon and social-card tag is on the home page, with absolute URLs.
#[tokio::test]
async fn the_home_page_has_meta_tags_and_social_cards() {
    let app = TestApp::new().await;
    let base = app.state.config.app_url.clone();
    let page = app.get("/").await;
    page.assert_ok()
        .assert_contains(r#"<meta name="description" content="A new Rust off Rails app."#)
        .assert_contains(&format!(r#"<link rel="canonical" href="{base}/">"#))
        .assert_contains(r#"<meta property="og:type" content="website">"#)
        .assert_contains(r#"<meta property="og:site_name" content="Offrails App">"#)
        .assert_contains(&format!(
            r#"<meta property="og:image" content="{base}/static/og.png">"#
        ))
        .assert_contains(r#"<meta property="og:image:width" content="1200">"#)
        .assert_contains(r#"<meta name="twitter:card" content="summary_large_image">"#)
        .assert_contains(r#"<link rel="icon" href="/favicon.ico" sizes="48x48">"#)
        .assert_contains(r#"<link rel="apple-touch-icon" href="/static/apple-touch-icon.png"#)
        .assert_not_contains("noindex");
    for path in [
        "/favicon.ico",
        "/static/favicon.svg",
        "/static/og.png",
        "/static/apple-touch-icon.png",
        "/static/icon-512.png",
    ] {
        app.get(path).await.assert_ok();
    }
}

#[tokio::test]
async fn robots_and_sitemap_are_absolute_and_exclude_private_pages() {
    let app = TestApp::new().await;
    let base = app.state.config.app_url.clone();
    app.get("/robots.txt")
        .await
        .assert_ok()
        .assert_contains("User-agent: *")
        .assert_contains("Disallow: /dev")
        .assert_contains(&format!("Sitemap: {base}/sitemap.xml"));
    app.get("/sitemap.xml")
        .await
        .assert_ok()
        .assert_contains(&format!("<loc>{base}/</loc>"))
        .assert_not_contains("/dev");
}

/// Every file under `dir` (recursively) with extension `ext`.
fn files_with_extension(dir: &std::path::Path, ext: &str, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_with_extension(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// The app layout boosts every link (`hx-boost="true"`): a boosted click swaps `<body>` and runs
/// nothing in the next page's `<head>`. So no page that extends the app layout loads a script or
/// a stylesheet in its `{% block head %}`; they load from the layout (`samples: app head`), where
/// every page has them (guides/frontend-htmx.md#boosted-navigation). The browser check is
/// `.offrails/tools/browser-smoke/smoke-boosted.mjs`.
#[test]
fn app_pages_load_no_scripts_or_stylesheets_in_their_head_block() {
    let mut files = Vec::new();
    files_with_extension(std::path::Path::new("templates"), "html", &mut files);
    let mut found = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("a readable template");
        if !source.contains(r#"{% extends "layouts/application.html" %}"#) {
            continue;
        }
        let Some(start) = source.find("{% block head %}") else {
            continue;
        };
        let head = &source[start..];
        let head = &head[..head.find("{% endblock").unwrap_or(head.len())];
        if head.contains("<script") || head.contains("stylesheet") {
            found.push(path.display().to_string());
        }
    }
    assert!(
        found.is_empty(),
        "a boosted link never runs these: load them from the layout (samples: app head): {found:?}"
    );
}

/// A script that binds on `DOMContentLoaded` (or `load`) binds once, on the first full load: after
/// a boosted link the new page's elements are never bound. Bind by delegation on `document`, or
/// also start on `htmx:load` for the elements a swap brings (guides/frontend-htmx.md#boosted-navigation).
#[test]
fn page_scripts_never_bind_only_on_dom_content_loaded() {
    let mut files = Vec::new();
    files_with_extension(std::path::Path::new("static"), "js", &mut files);
    let mut found = Vec::new();
    for path in files {
        // vendor/: third-party files. kernel/: kernel pages aren't boosted, and the dev
        // dashboard's load lab plants exactly this bug on purpose (`lab-widget.js`).
        if path
            .components()
            .any(|c| c.as_os_str() == "vendor" || c.as_os_str() == "kernel")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a readable script");
        let code: String = source
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let on_load = code.contains("DOMContentLoaded")
            || code.contains("window.onload")
            || code.contains(r#"addEventListener("load""#);
        if on_load && !code.contains("htmx:load") {
            found.push(path.display().to_string());
        }
    }
    assert!(
        found.is_empty(),
        "these bind on DOMContentLoaded/load only, so a boosted link leaves the next page dead: \
         delegate on document, or also start on htmx:load: {found:?}"
    );
}
