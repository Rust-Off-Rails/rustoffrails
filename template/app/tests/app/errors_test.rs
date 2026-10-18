//! .offrails/guides/errors-and-logging.md

use axum::Router;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::get;
use offrails_app::config::{AppEnv, Config};
use offrails_app::error::{self, AppError, ErrorReport};
use offrails_app::models::Errors;
use tower::ServiceExt;

use crate::support::TestApp;

fn status_of(error: AppError) -> StatusCode {
    error
        .into_response()
        .extensions()
        .get::<ErrorReport>()
        .expect("every AppError carries a report")
        .status
}

#[test]
fn every_app_error_maps_to_one_status() {
    assert_eq!(status_of(AppError::NotFound), StatusCode::NOT_FOUND);
    assert_eq!(status_of(AppError::Unauthorized), StatusCode::UNAUTHORIZED);
    assert_eq!(status_of(AppError::Forbidden), StatusCode::FORBIDDEN);
    assert_eq!(
        status_of(AppError::Invalid(Errors::default())),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        status_of(AppError::TooManyRequests),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        status_of(AppError::Unavailable),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status_of(AppError::Internal(anyhow::anyhow!("boom"))),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        status_of(sea_orm::DbErr::RecordNotFound("x".into()).into()),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status_of(sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout).into()),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status_of(sea_orm::DbErr::Custom("x".into()).into()),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

/// Going Forward from an error page must show the page, not a stale one: the page loads
/// `app.js`, whose `pageshow` handler reloads (guides/errors-and-logging.md).
#[tokio::test]
async fn error_pages_load_app_js() {
    let app = TestApp::new().await;
    app.get("/no-such-page")
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_contains(r#"<script src="/static/app.js" defer></script>"#);
    for (name, html) in [
        ("403", include_str!("../../templates/errors/403.html")),
        ("429", include_str!("../../templates/errors/429.html")),
        ("500", include_str!("../../templates/errors/500.html")),
        ("503", include_str!("../../templates/errors/503.html")),
    ] {
        assert!(
            html.contains(r#"<script src="/static/app.js" defer></script>"#),
            "{name}.html does not load app.js"
        );
    }
}

#[tokio::test]
async fn not_found_page_never_leaks_internals() {
    let app = TestApp::new().await;
    let res = app.get("/this-does-not-exist").await;
    res.assert_status(StatusCode::NOT_FOUND)
        .assert_contains("Page not found")
        .assert_not_contains("panic")
        .assert_not_contains("DATABASE_URL");
    assert_eq!(
        res.headers
            .get("x-route-fallback")
            .map(|v| v.to_str().unwrap()),
        Some("1")
    );
}

#[tokio::test]
async fn csrf_rejection_is_the_403_page() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await;
    app.without_fetch_metadata()
        .with_origin("https://evil.example");
    app.post_form("/dev/migrations", &[])
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_contains("Not allowed");
}

/// Every variant through `shape_errors` on a small router, per flavor.
async fn shaped(
    env: AppEnv,
    err: fn() -> AppError,
    headers: &[(&str, &str)],
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let app = TestApp::with_config(Config {
        env,
        ..Config::test()
    })
    .await;
    let router: Router = Router::new()
        .route("/{*rest}", get(move || async move { err() }))
        .layer(from_fn_with_state(app.state.clone(), error::shape_errors));
    let mut req = axum::http::Request::builder().uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = router
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn html_errors_render_pages_and_hide_internals_in_production() {
    let (s, _, body) = shaped(
        AppEnv::Production,
        || AppError::Internal(anyhow::anyhow!("db password=hunter2")),
        &[],
        "/x",
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("Something went wrong") && !body.contains("hunter2"));
    let (_, _, body) = shaped(
        AppEnv::Development,
        || AppError::Internal(anyhow::anyhow!("boom detail")),
        &[],
        "/x",
    )
    .await;
    assert!(body.contains("boom detail"), "development shows the chain");
}

#[tokio::test]
async fn unauthorized_redirects_html_and_hx_redirects_htmx() {
    let (s, h, _) = shaped(AppEnv::Test, || AppError::Unauthorized, &[], "/x").await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/session/new");
    let (s, h, _) = shaped(
        AppEnv::Test,
        || AppError::Unauthorized,
        &[("hx-request", "true")],
        "/x",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["hx-redirect"], "/session/new");
}

#[tokio::test]
async fn api_errors_are_json() {
    let (s, _, body) = shaped(AppEnv::Test, || AppError::Forbidden, &[], "/api/x").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    insta::assert_snapshot!(body, @r#"{"error":"forbidden"}"#);
    let (s, _, body) = shaped(
        AppEnv::Test,
        || AppError::Invalid(Errors::single("title", "can't be blank")),
        &[],
        "/api/x",
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    insta::assert_snapshot!(body, @r#"{"errors":{"title":["can't be blank"]}}"#);
    let (_, _, body) = shaped(AppEnv::Test, || AppError::Unauthorized, &[], "/api/x").await;
    insta::assert_snapshot!(body, @r#"{"error":"unauthorized"}"#);
}

#[tokio::test]
async fn invalid_on_htmx_keeps_the_url() {
    let (s, h, body) = shaped(
        AppEnv::Test,
        || AppError::Invalid(Errors::single("title", "can't be blank")),
        &[("hx-request", "true")],
        "/x",
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(h["hx-push-url"], "false");
    assert!(
        body.contains("Title can&#39;t be blank") || body.contains("Title can't be blank"),
        "{body}"
    );
}

#[tokio::test]
async fn unavailable_carries_retry_after() {
    let (s, h, body) = shaped(AppEnv::Test, || AppError::Unavailable, &[], "/x").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(h["retry-after"], "1");
    assert!(body.contains("Back in a moment"));
    let (_, h, _) = shaped(AppEnv::Test, || AppError::TooManyRequests, &[], "/x").await;
    assert_eq!(h["retry-after"], "180");
}

#[tokio::test]
async fn tasks_run_by_name_and_refuse_unknown_ones() {
    // Seeds are for development (a seed may refuse to run anywhere else), so the dispatcher is
    // proven under a development config, on the test database.
    let app = TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await;
    offrails_app::tasks::run(&app.state, "seed", vec![])
        .await
        .unwrap();
    assert!(
        offrails_app::tasks::run(&app.state, "nope", vec![])
            .await
            .is_err()
    );
}
