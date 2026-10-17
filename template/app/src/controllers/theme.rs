//! `POST /theme`: the theme switch without JavaScript. Sets the `offrails_theme` cookie and goes
//! back to the page it came from. With JS, `static/kernel/admin.js` writes the same cookie itself
//! and never posts here. CSRF: the router's `CsrfLayer` (Fetch Metadata / Origin), like every
//! POST. GUIDE: .offrails/guides/views.md#dark-mode

use axum::Form;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::kernel::theme::Theme;
use crate::kernel::ui;

#[derive(Debug, Deserialize)]
pub struct ThemeForm {
    #[serde(default)]
    theme: String,
    #[serde(default)]
    return_to: String,
}

pub async fn update(Form(form): Form<ThemeForm>) -> Response {
    // SECURITY: only a same-site path, never `//host` or `https://…`: no open redirect
    // (test: the_theme_switch_redirects_to_same_site_paths_only).
    let back = ui::same_site_path(&form.return_to).unwrap_or_else(|| "/".to_owned());
    let mut response = Redirect::to(&back).into_response();
    // An unknown value changes nothing (test: an_invalid_theme_is_ignored_by_the_switch).
    if let Some(theme) = Theme::parse(&form.theme) {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, theme.set_cookie());
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
