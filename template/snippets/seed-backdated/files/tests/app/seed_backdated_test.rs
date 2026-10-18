//! `seed_support::backdate` (the seed-backdated snippet), on a note from the resource snippet.

use chrono::Duration;
use offrails_app::models::{Page, note};
use offrails_app::tasks::seed_support::{backdate, days_ago};

use crate::factories;
use crate::support::TestApp;

#[tokio::test]
async fn a_backdated_note_is_older_and_lists_after_a_new_one() {
    let app = TestApp::new().await;
    let old = factories::note(app.db()).await;
    let new = factories::note(app.db()).await;
    backdate::<note::Entity>(app.db(), old.id, days_ago(30))
        .await
        .unwrap();
    let old = factories::reload_note(app.db(), old.id).await.unwrap();
    let age = new.created_at - old.created_at;
    assert!(
        age > Duration::days(29) && age < Duration::days(31),
        "{age}"
    );
    assert_eq!(old.updated_at, old.created_at);
    let (rows, _) = note::all(app.db(), Page::default()).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![new.id, old.id]
    );
}

#[tokio::test]
async fn backdating_a_missing_row_is_an_error() {
    let app = TestApp::new().await;
    assert!(
        backdate::<note::Entity>(app.db(), 999_999, days_ago(1))
            .await
            .is_err()
    );
}
