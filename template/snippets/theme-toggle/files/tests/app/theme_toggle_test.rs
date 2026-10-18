//! The theme toggle (the theme-toggle snippet): the button names the next theme, the no-JS POST
//! works, and static/app.js writes the cookie the server reads.

use axum::http::StatusCode;
use offrails_app::kernel::theme::{COOKIE, MAX_AGE_SECS};

use crate::support::TestApp;

#[tokio::test]
async fn the_toggle_offers_the_next_theme_on_every_page() {
    let app = TestApp::new().await;
    app.get("/")
        .await
        .assert_ok()
        .assert_contains("data-theme-toggle")
        .assert_contains("name=\"theme\" value=\"light\"")
        .assert_contains("name=\"return_to\" value=\"/\"");
    app.set_cookie(COOKIE, "dark");
    app.get("/")
        .await
        .assert_ok()
        .assert_contains("name=\"theme\" value=\"system\"");
}

#[tokio::test]
async fn without_js_the_toggle_posts_and_comes_back() {
    let app = TestApp::new().await;
    let res = app
        .post_form("/theme", &[("theme", "light"), ("return_to", "/")])
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(
        res.set_cookies()
            .iter()
            .any(|c| c.starts_with(&format!("{COOKIE}=light"))),
        "{:?}",
        res.set_cookies()
    );
}

// INVARIANT: the script and the server agree on the cookie (its name and lifetime).
#[test]
fn the_script_writes_the_cookie_the_server_reads() {
    let js = std::fs::read_to_string("static/app.js").expect("static/app.js");
    assert!(
        js.contains("[data-theme-toggle]"),
        "the handler is in static/app.js"
    );
    assert!(js.contains(&format!("{COOKIE}=${{theme}}")), "cookie name");
    assert!(
        js.contains(&format!("Max-Age={MAX_AGE_SECS}")),
        "cookie lifetime"
    );
}
