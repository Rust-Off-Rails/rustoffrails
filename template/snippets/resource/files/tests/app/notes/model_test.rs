//! `note` (src/models/note/) against the database: ordering, paging.
//! From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use offrails_app::error::AppError;
use offrails_app::models::{Page, note};

use crate::factories;
use crate::support::TestApp;

#[tokio::test]
async fn all_pages_newest_first() {
    let app = TestApp::new().await;
    let first = factories::note(app.db()).await;
    let second = factories::note(app.db()).await;
    let third = factories::note(app.db()).await;
    let (rows, more) = note::all(app.db(), Page::new(1, 2)).await.unwrap();
    assert_eq!(rows, vec![third, second]);
    assert!(more);
    let (rows, more) = note::all(app.db(), Page::new(2, 2)).await.unwrap();
    assert_eq!(rows, vec![first]);
    assert!(!more);
}

#[tokio::test]
async fn update_and_destroy() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    let new = factories::note_params(app.db()).await.validate().unwrap();
    let updated = note::update(app.db(), record.clone(), new).await.unwrap();
    assert_eq!(updated.id, record.id);
    note::destroy(app.db(), updated).await.unwrap();
    assert!(matches!(
        note::find(app.db(), record.id).await,
        Err(AppError::NotFound)
    ));
}

/// The edit form shows what was saved: params from a record validate back to it.
#[tokio::test]
async fn the_edit_form_round_trips_a_saved_note() {
    let app = TestApp::new().await;
    let record = factories::note(app.db()).await;
    let new = note::NoteParams::from(&record).validate().unwrap();
    let saved = note::update(app.db(), record.clone(), new).await.unwrap();
    assert_eq!(
        note::Model {
            updated_at: record.updated_at,
            ..saved
        },
        record
    );
}
