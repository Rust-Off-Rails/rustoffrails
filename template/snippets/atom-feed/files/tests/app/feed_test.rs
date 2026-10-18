//! The Atom feed (the atom-feed snippet). APP: cite the feed's AC ids (`/// AC-feed-1`).

use offrails_app::routes::paths;

use crate::support::TestApp;

#[tokio::test]
async fn the_feed_is_atom_and_public() {
    let app = TestApp::new().await;
    let res = app.get(&paths::feed()).await;
    res.assert_ok()
        .assert_contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">")
        .assert_contains("/feed.xml\" rel=\"self\"/>");
    assert_eq!(
        res.headers.get("content-type").unwrap(),
        "application/atom+xml; charset=utf-8"
    );
}

#[tokio::test]
async fn every_page_links_to_the_feed() {
    let app = TestApp::new().await;
    app.get("/")
        .await
        .assert_ok()
        .assert_contains("type=\"application/atom+xml\"")
        .assert_contains(&format!("href=\"{}\"", paths::feed()));
}
