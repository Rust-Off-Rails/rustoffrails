//! The public note page (the public-show-by-slug snippet): a note at its slug, the next slug for
//! the same title, 404 for an unknown one, markup shown as text.

use axum::http::StatusCode;
use offrails_app::models::note_slugs;
use offrails_app::routes::paths;

use crate::factories;
use crate::support::TestApp;

async fn titled(app: &TestApp, title: &str) -> offrails_app::models::note::Model {
    let mut params = factories::note_params(app.db()).await;
    params.title = title.into();
    let record = factories::note_with(app.db(), params).await;
    note_slugs::assign(app.db(), record).await.unwrap()
}

#[tokio::test]
async fn a_note_is_public_at_its_slug() {
    let app = TestApp::new().await;
    let record = titled(&app, "Hello, World!").await;
    assert_eq!(record.slug.as_deref(), Some("hello-world"));
    app.get(&paths::note_page("hello-world"))
        .await
        .assert_ok()
        .assert_text("Hello, World!")
        .assert_text(&record.body);
    app.get(&paths::note_page("no-such-note"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_same_title_gets_the_next_slug_and_a_slug_never_moves() {
    let app = TestApp::new().await;
    titled(&app, "Same title").await;
    let second = titled(&app, "Same title").await;
    assert_eq!(second.slug.as_deref(), Some("same-title-2"));
    let again = note_slugs::assign(app.db(), second.clone()).await.unwrap();
    assert_eq!(again.slug, second.slug);
}

#[tokio::test]
async fn markup_in_a_public_note_is_shown_as_text() {
    let app = TestApp::new().await;
    let record = titled(&app, "<script>alert(1)</script>").await;
    let slug = record.slug.unwrap();
    app.get(&paths::note_page(&slug))
        .await
        .assert_ok()
        .assert_not_contains("<script>alert(1)")
        .assert_text("<script>alert(1)</script>");
}
