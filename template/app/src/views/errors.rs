//! Error pages. Standalone documents: no layout, no `ctx`, no database. They link
//! `/static/app.css` without a digest, so they still render when everything else is broken.
//! `theme` is the request's `offrails_theme` cookie, so an error page keeps the chosen theme.

use askama::Template;

use crate::kernel::theme::Theme;

#[derive(Template)]
#[template(path = "errors/404.html")]
pub struct NotFound {
    pub theme: Theme,
}

#[derive(Template)]
#[template(path = "errors/403.html")]
pub struct Forbidden {
    pub theme: Theme,
}

#[derive(Template)]
#[template(path = "errors/429.html")]
pub struct TooManyRequests {
    pub theme: Theme,
}

#[derive(Template)]
#[template(path = "errors/503.html")]
pub struct Unavailable {
    pub theme: Theme,
}

/// `detail` (the error chain) is `Some` only in development.
#[derive(Template)]
#[template(path = "errors/500.html")]
pub struct ServerError {
    pub theme: Theme,
    pub detail: Option<String>,
}

/// The 422 fallback: a fragment listing full messages (handlers normally re-render the form).
#[derive(Template)]
#[template(path = "errors/_invalid.html")]
pub struct Invalid {
    pub messages: Vec<String>,
}
