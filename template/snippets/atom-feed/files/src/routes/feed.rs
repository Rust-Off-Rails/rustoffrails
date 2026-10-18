//! The Atom feed's route (the atom-feed snippet): its router, `paths::feed()` and its `ROUTES`
//! row. src/routes.rs wires it in with one `// atom-feed` line at each of four anchors.

use axum::{Router, routing::get};

use super::{Access, Route};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/feed.xml", get(crate::controllers::feed::show))
}

/// URL helpers; `crate::routes::paths` re-exports them.
pub mod paths {
    pub fn feed() -> String {
        "/feed.xml".into()
    }
}

#[rustfmt::skip]
pub const ROUTES: &[Route] = &[
    ("GET", "/feed.xml", "feed::show", Access::Public),
];
