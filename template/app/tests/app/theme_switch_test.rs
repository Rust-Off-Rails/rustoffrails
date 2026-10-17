//! The theme switch (src/kernel/theme.rs, controllers/theme.rs, static/kernel/admin.js): the
//! `offrails_theme` cookie becomes `<html data-theme>` on every page, the no-JS form is CSRF-checked
//! and only redirects to this site.

use axum::http::StatusCode;
use migration::{Migrator, MigratorTrait};
use offrails_app::config::{AppEnv, Config};

use crate::support::TestApp;

async fn dev_app() -> TestApp {
    TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await
}

/// Every kernel page (full HTML documents in the kernel layout). Samples add theirs.
const KERNEL_PAGES: &[&str] = &[
    "/dev",
    "/dev/routes",
    "/dev/system",
    // samples: kernel pages
];

fn html_tag(theme: &str) -> String {
    format!("<html lang=\"en\" data-theme=\"{theme}\">")
}

#[tokio::test]
async fn the_theme_cookie_sets_data_theme() {
    let app = dev_app().await;
    for theme in ["system", "light", "dark"] {
        app.set_cookie("offrails_theme", theme);
        app.get("/dev")
            .await
            .assert_ok()
            .assert_contains(&html_tag(theme))
            .assert_contains(&format!(
                "name=\"theme\" value=\"{theme}\" aria-pressed=\"true\""
            ));
        // The app layout and the error pages follow the same cookie.
        app.get("/").await.assert_contains(&html_tag(theme));
        app.get("/no/such/page")
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_contains(&html_tag(theme));
    }
    app.set_cookie("offrails_theme", "dark");
    app.get("/dev")
        .await
        .assert_contains("<meta name=\"color-scheme\" content=\"dark\">")
        .assert_contains("href=\"#ti-dark\" data-theme-icon");
}

#[tokio::test]
async fn an_invalid_theme_cookie_is_ignored() {
    let app = dev_app().await;
    for bad in ["blue", "Dark", "dark%22", "%22%3E%3Cscript%3E", ""] {
        app.set_cookie("offrails_theme", bad);
        app.get("/dev")
            .await
            .assert_ok()
            .assert_contains(&html_tag("system"))
            .assert_contains("<meta name=\"color-scheme\" content=\"light dark\">");
        app.get("/").await.assert_contains(&html_tag("system"));
    }
    app.clear_cookies();
    app.get("/dev").await.assert_contains(&html_tag("system"));
}

#[tokio::test]
async fn every_kernel_page_renders_data_theme() {
    let app = dev_app().await;
    for theme in ["system", "light", "dark"] {
        app.set_cookie("offrails_theme", theme);
        for path in KERNEL_PAGES {
            app.get(path)
                .await
                .assert_ok()
                .assert_contains(&html_tag(theme))
                .assert_contains("data-theme-switch")
                .assert_contains("action=\"/theme\"")
                .assert_contains(&format!("name=\"return_to\" value=\"{path}\""));
        }
    }
    // The pending-migrations page is a kernel page too, and its switch still works.
    Migrator::down(app.db(), None).await.unwrap();
    app.set_cookie("offrails_theme", "dark");
    app.get("/dev")
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .assert_contains(&html_tag("dark"));
    app.post_form("/theme", &[("theme", "light"), ("return_to", "/dev")])
        .await
        .assert_redirect_to("/dev");
    assert_eq!(app.cookie("offrails_theme").as_deref(), Some("light"));
}

#[tokio::test]
async fn the_theme_switch_sets_the_cookie_and_goes_back() {
    let app = TestApp::new().await;
    let res = app
        .post_form("/theme", &[("theme", "dark"), ("return_to", "/?a=1&b=2")])
        .await;
    res.assert_redirect_to("/?a=1&b=2");
    let cookies = res.set_cookies();
    let theme = cookies
        .iter()
        .find(|c| c.starts_with("offrails_theme="))
        .expect("a theme cookie");
    assert!(theme.starts_with("offrails_theme=dark; Path=/; SameSite=Lax; Max-Age=31536000"));
    assert!(!theme.to_ascii_lowercase().contains("httponly"));
    assert_eq!(
        res.headers
            .get("cache-control")
            .map(|v| v.to_str().unwrap()),
        Some("no-store")
    );
    app.get("/").await.assert_contains(&html_tag("dark"));
}

#[tokio::test]
async fn an_invalid_theme_is_ignored_by_the_switch() {
    let app = TestApp::new().await;
    app.set_cookie("offrails_theme", "dark");
    for bad in ["blue", "", "DARK", "dark; Path=/admin"] {
        let res = app
            .post_form("/theme", &[("theme", bad), ("return_to", "/")])
            .await;
        res.assert_redirect_to("/");
        assert!(
            res.set_cookies()
                .iter()
                .all(|c| !c.starts_with("offrails_theme=")),
            "{bad:?} set a cookie"
        );
    }
    assert_eq!(app.cookie("offrails_theme").as_deref(), Some("dark"));
}

#[tokio::test]
async fn the_theme_switch_redirects_to_same_site_paths_only() {
    let app = TestApp::new().await;
    for evil in [
        "//evil.example",
        "//evil.example/dev",
        "https://evil.example/",
        "http:evil.example",
        "/\\evil.example",
        "\\\\evil.example",
        "javascript:alert(1)",
        "/\t/evil.example",
        "evil.example",
        "",
    ] {
        app.post_form("/theme", &[("theme", "light"), ("return_to", evil)])
            .await
            .assert_redirect_to("/");
    }
    app.post_form(
        "/theme",
        &[("theme", "light"), ("return_to", "/dev/system")],
    )
    .await
    .assert_redirect_to("/dev/system");
}

#[tokio::test]
async fn the_theme_switch_requires_csrf() {
    let app = TestApp::new().await;
    for site in ["cross-site", "same-site"] {
        let res = app
            .post_form_with_headers(
                "/theme",
                &[("theme", "dark"), ("return_to", "/")],
                &[("sec-fetch-site", site)],
            )
            .await;
        res.assert_status(StatusCode::FORBIDDEN);
        assert!(
            res.set_cookies()
                .iter()
                .all(|c| !c.starts_with("offrails_theme="))
        );
    }
    // Without fetch metadata the Origin decides.
    app.without_fetch_metadata()
        .with_origin("https://evil.example");
    app.post_form("/theme", &[("theme", "dark"), ("return_to", "/")])
        .await
        .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(app.cookie("offrails_theme"), None);
}

#[tokio::test]
async fn the_theme_cookie_stays_script_writable_in_production() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    })
    .await;
    app.with_origin("https://app.example.com");
    let res = app
        .post_form("/theme", &[("theme", "dark"), ("return_to", "/")])
        .await;
    res.assert_redirect_to("/");
    let theme = res
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("offrails_theme="))
        .expect("a theme cookie");
    let lower = theme.to_ascii_lowercase();
    assert!(lower.contains("; secure"), "{theme}");
    assert!(!lower.contains("httponly"), "{theme}");
}

#[test]
fn the_theme_switch_script_and_server_agree() {
    let js = std::fs::read_to_string("static/kernel/admin.js").expect("admin.js");
    assert!(js.contains(&format!(
        "const COOKIE = \"{}\";",
        offrails_app::kernel::theme::COOKIE
    )));
    for theme in offrails_app::kernel::theme::Theme::ALL {
        assert!(js.contains(&format!("\"{}\"", theme.as_str())));
    }
    assert!(js.contains("new CustomEvent(\"themechange\""));
    let css = std::fs::read_to_string("static/kernel/admin.css").expect("admin.css");
    assert!(css.contains(":root[data-theme=\"light\"] { color-scheme: light; }"));
    assert!(css.contains(":root[data-theme=\"dark\"] { color-scheme: dark; }"));
    assert!(
        !css.contains("@media (prefers-color-scheme"),
        "a prefers-color-scheme block ignores the switch: define the token with light-dark()"
    );
}

/// Tailwind's `dark:` variant follows the switch too: `data-theme=dark` forces it, `light` blocks
/// it, and the OS decides otherwise (assets/app.css `@custom-variant dark`).
#[test]
fn tailwind_dark_utilities_follow_the_theme_switch() {
    use offrails_app::kernel::css::{CSS_BUILT, CSS_SOURCE};
    let source = std::fs::read_to_string(CSS_SOURCE).expect("assets/app.css");
    assert!(source.contains("@custom-variant dark {"));
    let Ok(built) = std::fs::read_to_string(CSS_BUILT) else {
        eprintln!("skipped: {CSS_BUILT} isn't built");
        return;
    };
    // The layout's body uses dark:bg-gray-950, so the build always has it.
    for selector in [
        r".dark\:bg-gray-950:where([data-theme=dark],[data-theme=dark] *)",
        r".dark\:bg-gray-950:where(:root:not([data-theme=light]),:root:not([data-theme=light]) *)",
    ] {
        assert!(
            built.contains(selector),
            "{CSS_BUILT} lacks {selector}: rebuild the CSS"
        );
    }
    assert!(
        built.contains("@media (prefers-color-scheme:dark){.dark\\:bg-gray-950:where(:root:not([data-theme=light])"),
        "the OS fallback must sit inside the prefers-color-scheme query"
    );
}
