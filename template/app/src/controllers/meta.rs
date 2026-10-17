//! `/robots.txt` and `/sitemap.xml` (guides/views.md#meta-favicon-and-social-cards).

use axum::{
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use crate::{routes::paths, state::AppState};

/// Public pages only, as absolute URLs built from `paths::` helpers.
fn public_paths() -> Vec<String> {
    vec![
        paths::root(),
        // samples: sitemap paths
    ]
}

fn base(state: &AppState) -> &str {
    state.config.app_url.trim_end_matches('/')
}

pub async fn robots(State(s): State<AppState>) -> Response {
    // Pages behind a login are private and never useful to a crawler: samples that add them
    // add their `Disallow:` lines here.
    let body = format!(
        "User-agent: *\nAllow: /\nDisallow: /dev\n\nSitemap: {}/sitemap.xml\n",
        base(&s)
    );
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

pub async fn sitemap(State(s): State<AppState>) -> Response {
    let urls: String = public_paths()
        .iter()
        .map(|p| format!("  <url><loc>{}{p}</loc></url>\n", base(&s)))
        .collect();
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
    );
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/xml; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}
