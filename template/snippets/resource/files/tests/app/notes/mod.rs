//! Notes (src/controllers/notes/): each action's happy path, validation.
//! From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).
//! APP: cite each acceptance criterion (`/// AC-notes-1`) on the test that proves it
//! (.offrails/guides/test-standards.md#1-traceability-specs--tests).

mod model_test;
mod read_test;
mod write_test;

use offrails_app::models::note;

use crate::support::{TestApp, TestResponse};

/// Submits `params` the way the form does.
async fn submit(app: &TestApp, path: &str, params: &note::NoteParams) -> TestResponse {
    let fields = [
        ("title", params.title.as_str()),
        ("body", params.body.as_str()),
    ];
    app.post_form(path, &fields).await
}
