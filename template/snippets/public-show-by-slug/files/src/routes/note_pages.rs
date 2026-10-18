//! The public note page's route (the public-show-by-slug snippet): its router, `paths::note_page`
//! and its `ROUTES` row. src/routes.rs wires it in with one marked line at each of four anchors.

use axum::{Router, routing::get};

use super::{Access, Route};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/read/notes/{slug}",
        get(crate::controllers::note_pages::show),
    )
}

/// URL helpers; `crate::routes::paths` re-exports them.
pub mod paths {
    pub fn note_page(slug: &str) -> String {
        format!("/read/notes/{slug}")
    }
}

#[rustfmt::skip]
pub const ROUTES: &[Route] = &[
    ("GET", "/read/notes/{slug}", "note_pages::show", Access::Public),
];
