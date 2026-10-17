//! Shared chrome for kernel pages (`/dev` and what samples add: the mailbox, `/admin/jobs`): the
//! layout context, `no_store()`, `page()`, and the time and number helpers. Server-rendered Askama
//! + vendored htmx + `static/kernel/admin.css`, identical in every `FRONTEND_MODE`.
//!
//! A kernel page that needs more than the shared stylesheet ships its own CSS/JS file and declares
//! it on its `Chrome` (`.stylesheet(..)`, `.script(..)`); the layout links exactly those. Nothing a
//! kernel page needs lives in `assets/app.css`, which kernel pages never load.

use std::sync::LazyLock;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use chrono::{DateTime, Utc};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::config::APP_NAME;
use crate::kernel::assets::Assets;
use crate::kernel::flash::FlashMessage;
use crate::kernel::theme::ThemeSwitch;

/// When this process booted (uptime on `/dev/system`). `AppState::new` forces it, so it is the
/// boot time, not the time of the first read.
pub static BOOTED_AT: LazyLock<DateTime<Utc>> = LazyLock::new(Utc::now);

/// Every response of a kernel router (pages, fragments, redirects, JSON, downloads) is
/// `no-store`: these pages show live data, so Back must never resurrect a stale snapshot.
pub fn no_store() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
}

/// Renders a kernel page.
pub fn page(template: impl askama::Template) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "kernel page failed to render");
            (StatusCode::INTERNAL_SERVER_ERROR, "kernel page error").into_response()
        }
    }
}

/// A tab in the kernel header.
pub struct Tab {
    pub key: &'static str,
    pub label: &'static str,
    pub href: &'static str,
    /// Development-only tabs are hidden (and their routes unmounted) elsewhere.
    pub dev_only: bool,
}

/// The kernel header's tabs, in order. Samples add theirs at the marker.
pub const TABS: &[Tab] = &[
    Tab {
        key: "dev",
        label: "Dev",
        href: "/dev",
        dev_only: true,
    },
    // samples: kernel tabs
    Tab {
        key: "routes",
        label: "Routes",
        href: "/dev/routes",
        dev_only: true,
    },
    Tab {
        key: "system",
        label: "System",
        href: "/dev/system",
        dev_only: true,
    },
];

/// Keys of `TABS` a sample replaces (a dashboard that supersedes System), hidden from the header.
/// The pages stay routed; only the tab goes.
pub const HIDDEN_TABS: &[&str] = &[
    // samples: hidden tabs
];

pub struct TabLink {
    pub label: &'static str,
    pub href: &'static str,
    pub active: bool,
}

/// Everything the kernel layout reads.
pub struct Chrome {
    pub title: String,
    pub app_name: &'static str,
    pub assets: Assets,
    pub tabs: Vec<TabLink>,
    pub flash: Option<FlashMessage>,
    /// The request's theme (`<html data-theme>`) and the header switch's return path.
    pub theme: ThemeSwitch,
    /// The page's own stylesheets and scripts, after `admin.css` / `admin.js`.
    pub stylesheets: Vec<String>,
    pub scripts: Vec<String>,
}

impl Chrome {
    pub fn new(
        title: impl Into<String>,
        active: &str,
        assets: Assets,
        development: bool,
        flash: Option<FlashMessage>,
        theme: ThemeSwitch,
    ) -> Self {
        Self {
            title: title.into(),
            app_name: APP_NAME,
            assets,
            tabs: TABS
                .iter()
                .filter(|t| (development || !t.dev_only) && !HIDDEN_TABS.contains(&t.key))
                .map(|t| TabLink {
                    label: t.label,
                    href: t.href,
                    active: t.key == active,
                })
                .collect(),
            flash,
            theme,
            stylesheets: Vec::new(),
            scripts: Vec::new(),
        }
    }

    /// A stylesheet this page needs (a digested `/static/...` URL from `AssetCatalog::path`).
    pub fn stylesheet(mut self, href: String) -> Self {
        self.stylesheets.push(href);
        self
    }

    /// A deferred script this page needs (a digested `/static/...` URL).
    pub fn script(mut self, src: String) -> Self {
        self.scripts.push(src);
        self
    }
}

/// `raw` when it is a path on this site, for `return_to` redirects: never `//host`,
/// `https://…`, a backslash or a control character, so no open redirect.
pub fn same_site_path(raw: &str) -> Option<String> {
    let ok = raw.starts_with('/')
        && !raw.starts_with("//")
        && !raw.contains('\\')
        && !raw.chars().any(char::is_control);
    ok.then(|| raw.to_owned())
}

/// "just now", "12s ago", "5m ago", "3h ago", "2d ago".
pub fn ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (now - then).num_seconds() {
        s if s < 0 => until(then, now),
        s if s < 5 => "just now".to_owned(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// "in 4s", "in 12m", "in 3h", "in 2d"; "now" when due.
pub fn until(when: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (when - now).num_seconds() {
        s if s <= 0 => "now".to_owned(),
        s if s < 60 => format!("in {s}s"),
        s if s < 3_600 => format!("in {}m", s / 60),
        s if s < 86_400 => format!("in {}h", s / 3_600),
        s => format!("in {}d", s / 86_400),
    }
}

/// "850ms", "2.4s", "3m 05s", "2h 10m".
pub fn duration(ms: i64) -> String {
    match ms {
        m if m < 1_000 => format!("{m}ms"),
        m if m < 60_000 => format!("{:.1}s", m as f64 / 1000.0),
        m if m < 3_600_000 => format!("{}m {:02}s", m / 60_000, (m % 60_000) / 1000),
        m => format!("{}h {:02}m", m / 3_600_000, (m % 3_600_000) / 60_000),
    }
}

/// "2026-10-12 21:04:05 UTC".
pub fn utc(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// At most `max` characters, with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// "1,234,567".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "512 B", "1.5 KB", "2.0 MB".
pub fn bytes(n: usize) -> String {
    match n {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn relative_times() {
        let now = Utc::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now - Duration::seconds(12), now), "12s ago");
        assert_eq!(ago(now - Duration::minutes(5), now), "5m ago");
        assert_eq!(ago(now - Duration::hours(3), now), "3h ago");
        assert_eq!(ago(now - Duration::days(2), now), "2d ago");
        assert_eq!(until(now + Duration::seconds(30), now), "in 30s");
        assert_eq!(until(now - Duration::seconds(1), now), "now");
    }

    #[test]
    fn durations_and_numbers() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(2_400), "2.4s");
        assert_eq!(duration(185_000), "3m 05s");
        assert_eq!(duration(7_800_000), "2h 10m");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(truncate("short", 80), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
    }

    #[test]
    fn production_chrome_has_no_dev_tabs() {
        let chrome = Chrome::new(
            "x",
            "dev",
            Assets::default(),
            false,
            None,
            ThemeSwitch::default(),
        );
        assert!(chrome.tabs.iter().all(|t| !t.href.starts_with("/dev")));
        let chrome = Chrome::new(
            "x",
            "dev",
            Assets::default(),
            true,
            None,
            ThemeSwitch::default(),
        );
        assert!(chrome.tabs.iter().any(|t| t.active && t.href == "/dev"));
    }

    #[test]
    fn return_paths_are_same_site_only() {
        assert_eq!(same_site_path("/books?q=a").as_deref(), Some("/books?q=a"));
        for bad in [
            "",
            "books",
            "//evil.example",
            "https://evil.example",
            "/\\evil.example",
            "/\tevil",
        ] {
            assert!(same_site_path(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_page_declares_its_own_assets() {
        let chrome = Chrome::new(
            "x",
            "dev",
            Assets::default(),
            true,
            None,
            ThemeSwitch::default(),
        )
        .stylesheet("/static/kernel/x.css?v=1".into())
        .script("/static/kernel/x.js?v=1".into());
        assert_eq!(chrome.stylesheets, ["/static/kernel/x.css?v=1"]);
        assert_eq!(chrome.scripts, ["/static/kernel/x.js?v=1"]);
    }
}
