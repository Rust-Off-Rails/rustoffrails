//! Page structs for notes. Display values are computed here from loaded rows; no
//! database access (.offrails/structure.md). From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use askama::Template;

use super::Ctx;
use crate::models::{Errors, note};
use crate::routes::paths;

/// One note as pages show it: every value a display string.
/// APP: times are shown in UTC; convert to the reader's zone here when the app knows it.
#[derive(Debug, Clone)]
pub struct NoteView {
    pub id: i64,
    pub label: String,
    pub title: String,
    pub body: String,
}

impl NoteView {
    pub fn from_model(r: &note::Model) -> Self {
        Self {
            id: r.id,
            label: r.label(),
            title: r.title.clone(),
            body: r.body.clone(),
        }
    }
}

/// `GET /notes`
#[derive(Template)]
#[template(path = "notes/index.html")]
pub struct NotesIndex {
    pub ctx: Ctx,
    pub notes: Vec<NoteView>,
    pub page: u64,
    pub more: bool,
}

impl NotesIndex {
    /// The pagination component's page count: this one, and the next when the query found more
    /// (no COUNT query).
    pub fn pages(&self) -> u64 {
        self.page + u64::from(self.more)
    }

    /// Where its links go (`{base}page=n`).
    pub fn page_base(&self) -> String {
        format!("{}?", paths::notes())
    }
}

/// `GET /notes/{id}`
#[derive(Template)]
#[template(path = "notes/show.html")]
pub struct NotesShow {
    pub ctx: Ctx,
    pub note: NoteView,
}

/// `GET /notes/new`, and a failed create (422).
#[derive(Template)]
#[template(path = "notes/new.html")]
pub struct NotesNew {
    pub ctx: Ctx,
    pub params: note::NoteParams,
    pub errors: Errors,
    pub action: String,
}

/// `GET /notes/{id}/edit`, and a failed update (422).
#[derive(Template)]
#[template(path = "notes/edit.html")]
pub struct NotesEdit {
    pub ctx: Ctx,
    pub id: i64,
    pub params: note::NoteParams,
    pub errors: Errors,
    pub action: String,
}
