//! `/dev` and the pending-migrations page. .offrails/guides/dev-tools.md

use axum::http::StatusCode;
use migration::{Migrator, MigratorTrait};
use offrails_app::config::{AppEnv, Config};

use crate::support::TestApp;

async fn dev_app() -> TestApp {
    TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await
}

#[tokio::test]
async fn every_dev_response_is_no_store() {
    let app = dev_app().await;
    for path in [
        "/dev",
        "/dev/routes",
        "/dev/system",
        "/dev/introspect.json",
        "/dev/reload",
        // samples: no-store paths
    ] {
        let res = app.get(path).await;
        assert_eq!(
            res.headers
                .get("cache-control")
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "path: {path}"
        );
    }
}

#[tokio::test]
async fn app_links_into_kernel_pages_are_not_boosted() {
    let app = dev_app().await;
    let res = app.get("/").await;
    res.assert_ok().assert_contains("href=\"/dev\"");
    for tag in res.body.split("<a ").skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if tag.contains("href=\"/dev") || tag.contains("href=\"/admin") {
            assert!(
                tag.contains("hx-boost=\"false\""),
                "a link into a kernel page is missing hx-boost=\"false\": <a {tag}>"
            );
        }
    }
}

#[tokio::test]
async fn dev_index_routes_and_system_pages_render() {
    let app = dev_app().await;
    app.get("/dev")
        .await
        .assert_ok()
        .assert_contains("Dev tools")
        .assert_contains("/static/kernel/admin.css?v=");
    app.get("/dev/routes")
        .await
        .assert_ok()
        .assert_contains("home::index");
    app.get("/dev/routes?q=introspect")
        .await
        .assert_ok()
        .assert_contains("dev::introspect")
        .assert_not_contains("home::index");
    app.get("/dev/system")
        .await
        .assert_ok()
        .assert_contains("development")
        .assert_contains("m20261016_000000_create_cache_entries");
}

/// Uptime counts from boot, not from the first time a page read the clock (it showed "0ms").
#[tokio::test]
async fn uptime_counts_from_boot_not_from_the_first_page_view() {
    let app = dev_app().await;
    // WHY: its own clock: reading BOOTED_AT here would start it and hide the bug.
    let booted = std::time::Instant::now();
    crate::support::wait_until("a second of uptime", async || {
        booted.elapsed() >= std::time::Duration::from_millis(1100)
    })
    .await;
    let page = app.get("/dev/system").await;
    page.assert_ok();
    let uptime = page
        .body
        .split("<dt>Uptime</dt><dd>")
        .nth(1)
        .and_then(|rest| rest.split('<').next())
        .expect("an uptime row");
    assert!(
        uptime.ends_with('s') && !uptime.ends_with("ms"),
        "uptime {uptime:?}"
    );
}

#[tokio::test]
async fn development_pages_load_dev_reload_and_others_do_not() {
    dev_app()
        .await
        .get("/")
        .await
        .assert_contains("/static/dev-reload.js");
    TestApp::new()
        .await
        .get("/")
        .await
        .assert_not_contains("dev-reload.js");
}

#[tokio::test]
async fn introspect_json_has_every_key_and_no_secrets() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Development,
        database_url: "sqlite::memory:".into(),
        ..Config::test()
    })
    .await;
    let res = app.get("/dev/introspect.json").await;
    res.assert_ok();
    let v: serde_json::Value = res.json();
    for key in [
        "app",
        "url",
        "config",
        "routes",
        "migrations",
        "assets",
        "css",
    ] {
        assert!(v.get(key).is_some(), "missing key: {key}");
    }
    assert!(
        v["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/dev/introspect.json")
    );
    assert_eq!(v["migrations"]["current"], true);
    assert!(!res.body.contains("postgres://"));
}

#[tokio::test]
async fn introspect_and_reload_are_404_outside_development() {
    for config in [
        Config::test(),
        Config {
            env: AppEnv::Production,
            ..Config::test()
        },
    ] {
        let app = TestApp::with_config(config).await;
        for path in ["/dev/introspect.json", "/dev/reload"] {
            app.get(path).await.assert_status(StatusCode::NOT_FOUND);
        }
    }
}

#[tokio::test]
async fn pending_migrations_show_a_503_page_with_a_run_button() {
    let app = dev_app().await;
    Migrator::down(app.db(), None).await.unwrap();

    let res = app.get("/?x=1").await;
    res.assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .assert_contains("pending migration")
        .assert_contains("create_cache_entries")
        .assert_contains("value=\"/?x=1\"")
        .assert_contains("/static/kernel/admin.css");
    // Static files and the agent endpoints still answer.
    app.get("/static/kernel/admin.css").await.assert_ok();
    app.get("/dev/reload").await.assert_ok();
    app.get("/up")
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);

    let run = app
        .post_form("/dev/migrations", &[("return_to", "//evil.example")])
        .await;
    run.assert_redirect_to("/dev/system");
    // The flash shows on the next page (any request would consume it, /up included).
    app.get("/dev/system")
        .await
        .assert_ok()
        .assert_contains(&format!("Ran {} migration", Migrator::migrations().len()));
    app.get("/up").await.assert_ok();
}

#[tokio::test]
async fn production_with_a_pending_migration_is_unhealthy_but_serves() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Production,
        ..Config::test()
    })
    .await;
    Migrator::down(app.db(), Some(1)).await.unwrap();
    app.get("/up")
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);
    // A static route: an app's `/` may read a table the pending migration creates.
    app.get("/robots.txt").await.assert_ok();
    app.post_form("/dev/migrations", &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}
