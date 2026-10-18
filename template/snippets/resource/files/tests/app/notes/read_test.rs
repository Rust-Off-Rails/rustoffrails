//! Notes: the list and one note. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use axum::http::StatusCode;
use offrails_app::routes::paths;

use crate::factories;
use crate::support::TestApp;

#[tokio::test]
async fn index_lists_notes() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    app.get(&paths::notes())
        .await
        .assert_ok()
        .assert_text(&record.label());
}

#[tokio::test]
async fn index_runs_the_same_queries_for_1_or_20_notes() {
    let app = TestApp::new().await;
    app.assert_query_count_stable(&paths::notes(), async |count| {
        for _ in 0..count {
            factories::note(app.db()).await;
        }
    })
    .await;
}

/// A page past the last one (or a huge `?page=`) is an empty list, never a 500.
#[tokio::test]
async fn a_page_past_the_end_is_empty() {
    let app = TestApp::new().await;
    for page in ["2", "18446744073709551615"] {
        app.get(&format!("{}?page={page}", paths::notes()))
            .await
            .assert_ok();
    }
}

#[tokio::test]
async fn show_renders_a_note() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    app.get(&paths::note(record.id))
        .await
        .assert_ok()
        .assert_text(&record.label());
    app.get(&paths::note(record.id + 1000))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}
