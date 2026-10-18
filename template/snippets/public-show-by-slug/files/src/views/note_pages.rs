//! The public note page's struct (the public-show-by-slug snippet).

use askama::Template;

use super::Ctx;
use super::notes::NoteView;
// The layout links with `paths::`.
use crate::routes::paths;

/// `GET /read/notes/{slug}`
#[derive(Template)]
#[template(path = "note_pages/show.html")]
pub struct NotePage {
    pub ctx: Ctx,
    pub note: NoteView,
}
