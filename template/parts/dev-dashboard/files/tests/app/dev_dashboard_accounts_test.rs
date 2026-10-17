//! The dev dashboard with auth-core installed (compiled only then, through the optional
//! `test mods` wire): the read-only production view's gate asks the session first.

use axum::http::StatusCode;
use base64::Engine;
use offrails_host::config::{AppEnv, Config, DashboardConfig};
use offrails_host::kernel::dashboard::access;

use crate::factories;
use crate::support::TestApp;

const ADMIN_PASSWORD: &str = "correct horse battery staple";

fn basic(password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("admin:{password}"))
    )
}

/// A signed-in person who isn't an admin gets 404 from `/admin/dashboard`, even with the right
/// Basic password: the page "doesn't exist" for them. Signed out, the Basic gate still works.
#[tokio::test]
async fn a_signed_in_non_admin_gets_404_from_the_production_view() {
    const { assert!(access::HAS_ACCOUNTS, "auth-core accounts gate the view") };
    let app = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        dashboard: DashboardConfig {
            in_production: true,
            admin_password: Some(ADMIN_PASSWORD.into()),
            ..DashboardConfig::default()
        },
        ..Config::test()
    })
    .await;
    let right = basic(ADMIN_PASSWORD);
    app.get_with_headers("/admin/dashboard", &[("authorization", &right)])
        .await
        .assert_ok();

    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    for path in [
        "/admin/dashboard",
        "/admin/dashboard/traces",
        "/admin/dashboard/sql",
    ] {
        app.get(path).await.assert_status(StatusCode::NOT_FOUND);
        app.get_with_headers(path, &[("authorization", &right)])
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
