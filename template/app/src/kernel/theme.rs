//! The color theme: System, Light or Dark, chosen with the switch in the kernel header and kept in
//! the `offrails_theme` cookie. The server renders `<html data-theme="…">` from it on every page
//! (kernel layout, app layout, error pages), so the first paint is already right: no flash, no
//! inline script. `static/kernel/admin.js` switches instantly, writes the same cookie and
//! dispatches `themechange` on `document`; without JS the switch is a POST form
//! (`controllers::theme::update`). CSS: tokens defined once with `light-dark()`, and
//! `:root[data-theme]` sets `color-scheme` (samples/FORMAT.md#theming).
//! GUIDE: .offrails/guides/views.md#dark-mode

use std::convert::Infallible;

use axum::extract::{FromRequestParts, OriginalUri};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Uri};

use crate::kernel::cookie;

/// The cookie's name. `static/kernel/admin.js` writes the same name.
pub const COOKIE: &str = "offrails_theme";

/// One year: a preference, not a session.
pub const MAX_AGE_SECS: i64 = 365 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the operating system (`prefers-color-scheme`).
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    /// The switch's order: System → Light → Dark.
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    /// The cookie value and the `data-theme` attribute.
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// APP: the switch's visible labels.
    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    /// `<meta name="color-scheme">`: what the browser paints before the CSS arrives.
    pub fn color_scheme(self) -> &'static str {
        match self {
            Theme::System => "light dark",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// The next one in the cycle System → Light → Dark → System.
    pub fn next(self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        }
    }

    /// Exact values only.
    pub fn parse(raw: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.as_str() == raw)
    }

    /// The request's theme. A missing or unknown cookie value means System.
    // INVARIANT: only the three known values ever reach a template; anything else is ignored
    // (test: an_invalid_theme_cookie_is_ignored).
    pub fn from_headers(headers: &HeaderMap) -> Theme {
        cookie::get(headers, COOKIE)
            .and_then(|v| Theme::parse(&v))
            .unwrap_or_default()
    }

    /// The `Set-Cookie` value. Not HttpOnly: `admin.js` rewrites it when JS switches.
    // WHY: a theme is not a secret, and a browser refuses to let a script overwrite an HttpOnly
    // cookie, so the no-JS form and the JS switch must write the same non-HttpOnly cookie
    // (security.rs keeps it out of the production HttpOnly backstop; test:
    // the_theme_cookie_stays_script_writable_in_production).
    pub fn set_cookie(self) -> HeaderValue {
        HeaderValue::from_str(&format!(
            "{COOKIE}={}; Path=/; SameSite=Lax; Max-Age={MAX_AGE_SECS}",
            self.as_str()
        ))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
    }
}

/// What the kernel header's switch needs: the current theme and the page to come back to after
/// the no-JS POST. An extractor, so a kernel handler takes it like the flash.
#[derive(Debug, Clone)]
pub struct ThemeSwitch {
    pub theme: Theme,
    /// This page's path and query (`return_to` in the form).
    pub return_to: String,
}

impl ThemeSwitch {
    pub fn new(headers: &HeaderMap, uri: &Uri) -> Self {
        Self {
            theme: Theme::from_headers(headers),
            return_to: uri
                .path_and_query()
                .map_or("/", |pq| pq.as_str())
                .to_owned(),
        }
    }

    /// `aria-pressed` for the option `value`.
    pub fn pressed(&self, value: &str) -> &'static str {
        if self.theme.as_str() == value {
            "true"
        } else {
            "false"
        }
    }
}

impl Default for ThemeSwitch {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            return_to: "/".to_owned(),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ThemeSwitch {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        // WHY: inside a nested router `parts.uri` lacks the prefix; the original URI is the page.
        let uri = parts
            .extensions
            .get::<OriginalUri>()
            .map_or_else(|| parts.uri.clone(), |o| o.0.clone());
        Ok(Self::new(&parts.headers, &uri))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;

    fn with_cookie(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("a=1; {COOKIE}={value}")).unwrap(),
        );
        h
    }

    #[test]
    fn only_known_values_parse() {
        for t in Theme::ALL {
            assert_eq!(Theme::parse(t.as_str()), Some(t));
        }
        for bad in ["", "Dark", "dark ", "blue", "\"><script>", "system;"] {
            assert_eq!(Theme::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Theme::from_headers(&with_cookie("dark")), Theme::Dark);
        assert_eq!(Theme::from_headers(&with_cookie("evil")), Theme::System);
        assert_eq!(Theme::from_headers(&HeaderMap::new()), Theme::System);
    }

    #[test]
    fn the_cycle_visits_every_theme() {
        assert_eq!(Theme::System.next().next().next(), Theme::System);
        assert_eq!(Theme::System.next(), Theme::Light);
        assert_eq!(Theme::Light.next(), Theme::Dark);
    }

    #[test]
    fn the_cookie_is_lax_long_lived_and_script_writable() {
        let c = Theme::Dark.set_cookie();
        let c = c.to_str().unwrap();
        assert!(c.starts_with("offrails_theme=dark; Path=/; SameSite=Lax; Max-Age="));
        assert!(!c.to_ascii_lowercase().contains("httponly"));
    }
}
