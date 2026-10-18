//! The layouts and the component kit (.offrails/guides/views.md#layouts, #components): every
//! layout has the skip link, landmarks and one shared <head>; the gallery's demo form keeps what
//! was typed on a 422; the gallery is development only.

use axum::http::StatusCode;
use offrails_app::config::{AppEnv, Config};

use crate::support::TestApp;

async fn dev_app() -> TestApp {
    TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await
}

/// The part of the page before `</head>`.
fn head(body: &str) -> &str {
    body.split("</head>").next().unwrap_or_default()
}

#[tokio::test]
async fn every_layout_has_the_skip_link_landmarks_and_the_same_head() {
    let app = dev_app().await;
    let top = app.get("/").await;
    top.assert_ok();
    let shared = head(&top.body).to_owned();
    for path in [
        "/",
        "/dev/components",
        "/dev/layouts/sidebar",
        "/dev/layouts/marketing",
        "/dev/layouts/auth",
        "/dev/layouts/docs",
    ] {
        let page = app.get(path).await;
        page.assert_ok()
            .assert_contains(r##"<a href="#main" class="skip-link">Skip to content</a>"##)
            .assert_contains(r#"<main id="main""#)
            .assert_contains(r#"<body hx-boost="true""#)
            .assert_contains(r#"id="confirm-dialog""#)
            .assert_contains(r#"id="i-x""#);
        assert_eq!(page.body.matches("<main").count(), 1, "{path}: one <main>");
        // A boosted link swaps <body> only: every page's scripts are the layout's.
        let scripts = |h: &str| h.matches("<script").count();
        assert_eq!(
            scripts(head(&page.body)),
            scripts(&shared),
            "{path}: page-specific scripts in <head>"
        );
        assert!(
            head(&page.body).contains("/static/components.js?v="),
            "{path}"
        );
        if path != "/dev/layouts/auth" {
            assert!(
                page.body.contains(r#"aria-label="Main""#),
                "{path}: a main navigation landmark"
            );
        }
    }
}

#[tokio::test]
async fn the_gallery_form_keeps_what_was_typed_on_a_422() {
    let app = dev_app().await;
    let res = app
        .post_form(
            "/dev/components",
            &[
                ("title", "Dune & co"),
                ("email", "nope"),
                ("password", "secret-pw"),
                ("plan", "team"),
                ("contact", "phone"),
            ],
        )
        .await;
    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_contains(r#"value="Dune &#38; co""#)
        .assert_contains(r#"value="team" selected"#)
        .assert_contains(r#"value="phone" checked"#)
        .assert_contains(r#"aria-describedby="email-error""#)
        .assert_text("Email must be an email address")
        .assert_text("I accept the terms must be accepted")
        .assert_not_contains("secret-pw");
    let ok = app
        .post_form(
            "/dev/components",
            &[
                ("title", "Dune"),
                ("email", "a@example.com"),
                ("plan", "free"),
                ("terms", "true"),
            ],
        )
        .await;
    ok.assert_redirect_to("/dev/components");
}

#[tokio::test]
async fn the_gallery_pins_a_theme_and_sorts_and_pages_its_table() {
    let app = dev_app().await;
    app.get("/dev/components?theme=dark")
        .await
        .assert_ok()
        .assert_contains(r#"<html lang="en" data-theme="dark">"#);
    let sorted = app.get("/dev/components?sort=year&dir=desc&page=6").await;
    sorted
        .assert_ok()
        .assert_contains(r#"aria-sort="descending""#)
        .assert_contains(r#"aria-current="page">6</a>"#)
        .assert_contains(r#"rel="prev""#);
    let body = &sorted.body;
    assert!(
        body.find("Piranesi").unwrap() < body.find("Dune").unwrap(),
        "2020 before 1965"
    );
    app.get("/dev/components/modal")
        .await
        .assert_ok()
        .assert_contains("data-modal-autoshow");
}

#[tokio::test]
async fn the_auth_card_shows_its_error_state_and_keeps_the_email() {
    let app = dev_app().await;
    app.post_form(
        "/dev/layouts/auth",
        &[("email", "ada@example.com"), ("password", "x")],
    )
    .await
    .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
    .assert_contains(r#"value="ada@example.com""#)
    .assert_contains(r#"aria-invalid="true""#);
}

#[tokio::test]
async fn the_gallery_exists_in_development_only() {
    for path in ["/dev/components", "/dev/layouts/sidebar"] {
        TestApp::new()
            .await
            .get(path)
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
