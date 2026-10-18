//! `GET /feed.xml`: an Atom feed of the newest entries (the atom-feed snippet).
//! APP: say what the feed holds (product/features/feed.md), and load it in `entries`.

use askama::Template;
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

use crate::{
    error::AppError,
    state::AppState,
    views::feed::{Entry, Feed},
};

/// The newest entries, newest first, at most `Feed::MAX`. APP: load them from your model, e.g.
/// `note::all(&s.db, Page::new(1, 20))`, each row an `Entry { title, path, updated, summary }`,
/// with `path` from `paths::` (only public records: a feed is public).
async fn entries(s: &AppState) -> Result<Vec<Entry>, AppError> {
    let _ = &s.db;
    Ok(Vec::new())
}

/// `GET /feed.xml`
#[tracing::instrument(skip_all)]
pub async fn show(State(s): State<AppState>) -> Result<Response, AppError> {
    let feed = Feed::new(
        crate::config::APP_NAME,
        &s.config.app_url,
        entries(&s).await?,
    );
    Ok((
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        feed.render()?,
    )
        .into_response())
}
