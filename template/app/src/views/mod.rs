//! What the client receives: Askama page structs, one file per resource, plus the layout context.

pub mod errors;
pub mod gallery;
pub mod helpers;
pub mod home;
// samples: view mods

use crate::error::AppError;
use crate::kernel::assets::Assets;
use crate::kernel::flash::FlashMessage;

pub const DEFAULT_DESCRIPTION: &str = "A new Rust off Rails app.";

pub fn render(t: impl askama::Template) -> Result<axum::response::Html<String>, AppError> {
    Ok(axum::response::Html(t.render()?))
}

/// The layout context every page struct carries. The layout reads only from `ctx`.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub title: String,
    pub description: String,
    pub path: String,
    pub app_url: String,
    pub flash: Option<FlashMessage>,
    pub assets: Assets,
    pub dev: bool,
    pub noindex: bool,
    /// Development only: static/app.css is missing, empty or stale (dev-tools.md#introspection).
    pub css_warning: Option<String>,
    /// `<html data-theme>`: the `offrails_theme` cookie (kernel/theme.rs).
    pub theme: crate::kernel::theme::Theme,
    // samples: ctx fields
}

impl Ctx {
    pub fn titled(mut self, t: &str) -> Self {
        self.title = t.to_owned();
        self
    }

    /// Collapses whitespace and caps at 300 chars (search engines show ~160).
    pub fn described(mut self, d: &str) -> Self {
        let collapsed: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
        self.description = collapsed.chars().take(300).collect();
        self
    }

    /// Pages behind a login: `<meta name="robots" content="noindex">`.
    pub fn noindex(mut self) -> Self {
        self.noindex = true;
        self
    }

    /// "Posts · My App", or "My App: <tagline>" on the home page.
    pub fn page_title(&self) -> String {
        if self.title.is_empty() {
            format!(
                "{}: {}",
                crate::config::APP_NAME,
                crate::config::APP_TAGLINE
            )
        } else {
            format!("{} · {}", self.title, crate::config::APP_NAME)
        }
    }

    pub fn canonical(&self) -> String {
        format!("{}{}", self.app_url, self.path)
    }

    pub fn absolute(&self, path: &str) -> String {
        format!("{}{path}", self.app_url)
    }
}
