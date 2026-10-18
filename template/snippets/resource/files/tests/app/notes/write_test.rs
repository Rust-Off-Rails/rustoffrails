//! Notes: the forms, saving, and deleting. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use axum::http::StatusCode;
use offrails_app::models::note;
use offrails_app::routes::paths;

use super::submit;
use crate::factories;
use crate::support::TestApp;

#[tokio::test]
async fn new_renders_the_form() {
    let app = TestApp::new().await;
    app.get(&paths::new_note())
        .await
        .assert_ok()
        .assert_contains(&format!("action=\"{}\"", paths::notes()));
}

#[tokio::test]
async fn create_saves_and_redirects_to_the_note() {
    let app = TestApp::new().await;
    let params = factories::note_params(app.db()).await;
    let res = submit(&app, &paths::notes(), &params).await;
    let (rows, _) = note::all(app.db(), offrails_app::models::Page::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one note saved");
    res.assert_redirect_to(&paths::note(rows[0].id));
    app.get(&paths::note(rows[0].id))
        .await
        .assert_ok()
        .assert_text("Note was successfully created.");
}

#[tokio::test]
async fn create_with_invalid_params_is_422_with_the_errors_and_the_input() {
    let app = TestApp::new().await;
    let mut params = factories::note_params(app.db()).await;
    params.title = "".into();
    let res = submit(&app, &paths::notes(), &params).await;
    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("Title can't be blank");
    let (rows, _) = note::all(app.db(), offrails_app::models::Page::default())
        .await
        .unwrap();
    assert!(rows.is_empty(), "nothing saved");
}

#[tokio::test]
async fn edit_shows_the_saved_values_and_update_saves_changes() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    app.get(&paths::edit_note(record.id))
        .await
        .assert_ok()
        .assert_contains(&format!("action=\"{}\"", paths::note(record.id)));
    let mut params = note::NoteParams::from(&record);
    params.title = "Changed by the test".into();
    submit(&app, &paths::note(record.id), &params)
        .await
        .assert_redirect_to(&paths::note(record.id));
    app.get(&paths::note(record.id))
        .await
        .assert_ok()
        .assert_text("Note was successfully updated.")
        .assert_text("Changed by the test");
}

#[tokio::test]
async fn update_with_invalid_params_is_422_and_changes_nothing() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    let mut params = note::NoteParams::from(&record);
    params.title = "".into();
    submit(&app, &paths::note(record.id), &params)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("Title can't be blank");
    assert_eq!(
        factories::reload_note(app.db(), record.id).await,
        Some(record)
    );
}

#[tokio::test]
async fn destroy_deletes_and_redirects_to_the_list() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    app.post_form(&paths::delete_note(record.id), &[])
        .await
        .assert_redirect_to(&paths::notes());
    assert!(factories::reload_note(app.db(), record.id).await.is_none());
    app.get(&paths::note(record.id))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn markup_in_a_note_is_shown_as_text() {
    let app = TestApp::new().await;
    let mut params = factories::note_params(app.db()).await;
    params.title = "<script>alert(1)</script>".into();
    let record = factories::note_with(app.db(), params).await;
    for path in [paths::notes(), paths::note(record.id)] {
        app.get(&path)
            .await
            .assert_ok()
            .assert_not_contains("<script>alert(1)")
            .assert_text("<script>alert(1)</script>");
    }
}
