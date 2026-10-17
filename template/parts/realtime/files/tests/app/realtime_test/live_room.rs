//! The live room's pages, the htmx refresh helper, development guests and /dev/sockets.

use axum::http::StatusCode;
use offrails_host::channels::identity;
use offrails_host::config::{AppEnv, Config};
use offrails_host::models::{room, room_message};
use serde_json::{Value, json};

use super::client::{ORIGIN, Ws, guest, serve};
use crate::factories;
use crate::support::TestApp;

fn development() -> Config {
    Config {
        env: AppEnv::Development,
        ..Config::test()
    }
}

fn production() -> Config {
    Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    }
}

#[tokio::test]
async fn the_room_renders_its_history_escaped_with_the_cursor_the_socket_resumes_from() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let lobby = factories::find_room(app.db(), "lobby").await;
    let now = chrono::Utc::now();
    room_message::create(app.db(), lobby.id, ann.id, "Ann", "first".into(), now)
        .await
        .unwrap();
    let last = room_message::create(
        app.db(),
        lobby.id,
        ann.id,
        "Ann",
        "<i>second</i>".into(),
        now,
    )
    .await
    .unwrap();
    app.set_cookie("session_token", &ann.token);
    let page = app.get("/live/lobby").await;
    page.assert_ok()
        .assert_text("first")
        .assert_text("<i>second</i>")
        .assert_not_contains("<i>second</i>")
        .assert_contains(&format!(r#"data-since="{}""#, last.id))
        .assert_contains(r#"data-topic="room:lobby""#)
        .assert_contains(&format!(r#"data-user-topic="user:{}""#, ann.id))
        .assert_contains("/static/channels.js?v=")
        .assert_contains("/static/live_room.js?v=");
    app.get("/live").await.assert_redirect_to("/live/lobby");
}

#[tokio::test]
async fn signed_out_visitors_are_sent_to_sign_in() {
    let app = TestApp::new().await;
    app.get("/live/lobby")
        .await
        .assert_redirect_to("/session/new");
    app.get("/live").await.assert_redirect_to("/session/new");
    // Development signs guests in; with accounts (auth-core) it's the app's sign-in there too.
    let dev = TestApp::with_config(development()).await;
    dev.get("/live/lobby")
        .await
        .assert_redirect_to(if identity::DEV_GUESTS {
            "/dev/guests/sign-in"
        } else {
            "/session/new"
        });
}

#[tokio::test]
async fn a_members_only_room_is_not_found_for_others() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    app.set_cookie("session_token", &ann.token);
    app.get("/live/backstage")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.get("/live/no-such-room")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.get_htmx("/live/backstage/header")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.get("/live/lobby")
        .await
        .assert_not_contains("/live/backstage");

    let backstage = factories::find_room(app.db(), "backstage").await;
    room::add_member(app.db(), backstage.id, ann.id, chrono::Utc::now())
        .await
        .unwrap();
    app.get("/live/backstage")
        .await
        .assert_ok()
        .assert_text("Members only");
    app.get("/live/lobby")
        .await
        .assert_contains("/live/backstage");
}

#[tokio::test]
async fn editing_the_headline_refreshes_the_header_on_every_open_page() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut watcher = Ws::as_guest(addr, &bob).await;
    watcher.join("room:lobby", None).await.unwrap();

    app.set_cookie("session_token", &ann.token);
    let res = app
        .post_form_with_headers(
            "/live/lobby/headline",
            &[("headline", "Ship it <today>")],
            &[("hx-request", "true")],
        )
        .await;
    res.assert_ok()
        .assert_text("Ship it <today>")
        .assert_contains(r#"id="room-header""#);
    let refresh = watcher.event("room:lobby", "refresh").await;
    assert_eq!(refresh["data"], json!({ "region": "room-header" }));
    assert!(refresh.get("seq").is_none());

    // What the watcher's page then fetches (htmx, through the normal authorized GET):
    app.set_cookie("session_token", &bob.token);
    app.get_htmx("/live/lobby/header")
        .await
        .assert_ok()
        .assert_text("Ship it <today>");
    app.get("/live/lobby/header")
        .await
        .assert_redirect_to("/live/lobby");

    let res = app
        .post_form_with_headers(
            "/live/lobby/headline",
            &[("headline", &"x".repeat(121))],
            &[("hx-request", "true")],
        )
        .await;
    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("Headline is too long (maximum is 120 characters)");
    assert_eq!(
        factories::find_room(app.db(), "lobby").await.headline,
        "Ship it <today>"
    );
}

#[tokio::test]
async fn development_guests_sign_in_and_signing_out_closes_their_sockets() {
    if !identity::DEV_GUESTS {
        // Accounts replaced the guests: accounts.rs proves their sign-out and the guest pages.
        return;
    }
    let app = TestApp::with_config(development()).await;
    app.get("/dev/guests/sign-in")
        .await
        .assert_ok()
        .assert_text("Display name");
    app.post_form("/dev/guests/sign-in", &[("name", "  ")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("Name can't be blank");
    let res = app
        .post_form("/dev/guests/sign-in", &[("name", "Blake Example")])
        .await;
    res.assert_redirect_to("/live/lobby");
    assert!(
        res.set_cookies()
            .iter()
            .any(|c| c.starts_with("session_token=")
                && c.contains("HttpOnly")
                && c.contains("SameSite=Lax"))
    );
    let token = app.cookie("session_token").expect("signed in");
    app.get("/live/lobby")
        .await
        .assert_ok()
        .assert_text("Blake Example");

    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::connect(addr, Some(&format!("session_token={token}")), Some(ORIGIN))
        .await
        .unwrap();
    ws.join("room:lobby", None).await.unwrap();
    app.post_form("/dev/guests/sign-out", &[])
        .await
        .assert_redirect_to("/dev/guests/sign-in");
    assert_eq!(ws.closed().await, (4401, Some("revoked".into())));
    assert!(app.cookie("session_token").is_none());
    // The old cookie, replayed, signs nobody in.
    app.set_cookie("session_token", &token);
    app.get("/live/lobby")
        .await
        .assert_redirect_to("/dev/guests/sign-in");
}

#[tokio::test]
async fn development_guests_and_dev_sockets_do_not_exist_in_production_or_test() {
    for config in [production(), Config::test()] {
        let app = TestApp::with_config(config).await;
        for path in [
            "/dev/guests/sign-in",
            "/dev/sockets",
            "/dev/sockets.json",
            "/dev/sockets/live",
        ] {
            app.get(path).await.assert_status(StatusCode::NOT_FOUND);
        }
        app.post_form("/dev/guests/sign-in", &[("name", "Intruder")])
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn dev_sockets_shows_connections_topics_presence_and_frames() {
    let app = TestApp::with_config(development()).await;
    let ann = guest(&app.state, "Ann Example").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    ws.push("room:lobby", "post", json!({ "body": "logged" }))
        .await
        .unwrap();

    let page = app.get("/dev/sockets").await;
    page.assert_ok()
        .assert_contains("/static/kernel/sockets.css?v=")
        .assert_text("room:lobby")
        .assert_text(&ann.name)
        .assert_contains("websocket")
        .assert_contains(r#"hx-trigger="every 2s""#);
    let snap: Value = app.get("/dev/sockets.json").await.json();
    let conn = snap["conns"][0]["id"].as_u64().unwrap();
    assert_eq!(snap["conns"][0]["transport"], "websocket");
    assert_eq!(snap["topics"][0]["people"], json!([ann.name]));
    let frames = app.get(&format!("/dev/sockets?conn={conn}")).await;
    frames
        .assert_ok()
        .assert_text(r#""event":"post""#)
        .assert_text("logged");
    app.get_htmx(&format!("/dev/sockets/live?conn={conn}"))
        .await
        .assert_ok()
        .assert_text("Frames of #");
    app.get("/dev")
        .await
        .assert_ok()
        .assert_text("Sockets")
        .assert_text("1 connection");
    let introspect: Value = app.get("/dev/introspect.json").await.json();
    assert_eq!(introspect["realtime"]["connections"], 1);
}

#[tokio::test]
async fn every_sockets_response_is_no_store() {
    let app = TestApp::with_config(development()).await;
    for path in [
        "/dev/sockets",
        "/dev/sockets.json",
        "/dev/sockets/live",
        "/dev/sockets?conn=1",
    ] {
        let res = app.get(path).await;
        assert_eq!(res.headers["cache-control"], "no-store", "{path}");
    }
}

#[tokio::test]
async fn links_into_the_live_room_and_dev_sockets_are_not_boosted() {
    let app = TestApp::with_config(development()).await;
    let home = app.get("/").await;
    home.assert_ok();
    for href in ["href=\"/live\"", "href=\"/dev/sockets\""] {
        let tag = home
            .body
            .split("<a ")
            .find(|t| t.contains(href))
            .unwrap_or_else(|| panic!("a link {href}"));
        assert!(
            tag.split('>')
                .next()
                .unwrap()
                .contains("hx-boost=\"false\""),
            "{href}"
        );
    }
}

/// The room, the guest sign-in and /dev/sockets follow the kernel header's theme switch: the
/// server renders `data-theme` from the `offrails_theme` cookie, and no stylesheet of this sample
/// has a `prefers-color-scheme` color block (samples/FORMAT.md#theming).
#[tokio::test]
async fn the_live_room_and_dev_sockets_follow_the_theme_switch() {
    let app = TestApp::with_config(development()).await;
    for theme in ["light", "dark"] {
        let tag = format!(r#"<html lang="en" data-theme="{theme}">"#);
        app.clear_cookies();
        app.set_cookie("offrails_theme", theme);
        if identity::DEV_GUESTS {
            app.get("/dev/guests/sign-in")
                .await
                .assert_ok()
                .assert_contains(&tag);
        }
        app.get("/dev/sockets")
            .await
            .assert_ok()
            .assert_contains(&tag)
            .assert_contains("data-theme-switch");
        let ann = guest(&app.state, &format!("Ann {theme}")).await;
        app.set_cookie("session_token", &ann.token);
        app.get("/live/lobby")
            .await
            .assert_ok()
            .assert_contains(&tag);
    }
    let css = std::fs::read_to_string("static/kernel/sockets.css").unwrap();
    assert!(
        !css.contains("@media (prefers-color-scheme"),
        "a color-scheme media query ignores the switch: use the kernel's light-dark() tokens"
    );
}
