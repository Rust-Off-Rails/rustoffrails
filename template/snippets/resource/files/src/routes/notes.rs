//! Notes (`cargo offrails generate resource notes`): its router, `paths::` helpers and `ROUTES` rows.
//! src/routes.rs wires it in with one `// notes` line at `route mods`, `routes`, `paths`, `route tables`.

use axum::{
    Router,
    routing::{get, post},
};

use super::{Access, Route};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/notes",
            get(crate::controllers::notes::index).post(crate::controllers::notes::create),
        )
        .route("/notes/new", get(crate::controllers::notes::new))
        .route(
            "/notes/{id}",
            get(crate::controllers::notes::show).post(crate::controllers::notes::update),
        )
        .route("/notes/{id}/edit", get(crate::controllers::notes::edit))
        .route(
            "/notes/{id}/delete",
            post(crate::controllers::notes::destroy),
        )
}

/// URL helpers; `crate::routes::paths` re-exports them.
pub mod paths {
    pub fn notes() -> String {
        "/notes".into()
    }

    pub fn new_note() -> String {
        "/notes/new".into()
    }

    pub fn note(id: i64) -> String {
        format!("/notes/{id}")
    }

    pub fn edit_note(id: i64) -> String {
        format!("/notes/{id}/edit")
    }

    pub fn delete_note(id: i64) -> String {
        format!("/notes/{id}/delete")
    }
}

#[rustfmt::skip]
pub const ROUTES: &[Route] = &[
    ("GET",  "/notes",             "notes::index",   Access::Public),
    ("GET",  "/notes/new",         "notes::new",     Access::Public),
    ("POST", "/notes",             "notes::create",  Access::Public),
    ("GET",  "/notes/{id}",        "notes::show",    Access::Public),
    ("GET",  "/notes/{id}/edit",   "notes::edit",    Access::Public),
    ("POST", "/notes/{id}",        "notes::update",  Access::Public),
    ("POST", "/notes/{id}/delete", "notes::destroy", Access::Public),
];
