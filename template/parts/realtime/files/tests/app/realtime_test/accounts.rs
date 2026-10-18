//! Realtime with auth-core's accounts (compiled only when auth-core is installed, through the
//! optional `realtime test mods` wire): people come from auth-core sessions, signing out through
//! auth-core closes that session's sockets at once, presence shows a name and never the address,
//! and the development guest pages send people to sign in.

use axum::http::{HeaderMap, HeaderValue, header};
use offrails_host::config::{AppEnv, Config};
use offrails_host::controllers::auth::{self, RequestMeta};
use offrails_host::models::{auth_event::Meta, user};
use offrails_host::routes::paths;
use serde_json::json;

use super::client::{Ws, guest, serve};
use crate::support::TestApp;

#[tokio::test]
async fn signing_out_through_auth_core_closes_that_sessions_sockets_at_once() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let ann_phone = ann.second_session(&app.state).await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut laptop = Ws::as_guest(addr, &ann).await;
    let mut phone = Ws::as_guest(addr, &ann_phone).await;
    laptop.join("room:lobby", None).await.unwrap();
    // The revalidation interval is 60 s; closing within the test's wait proves the wake-up
    // (auth::sessions_ended -> channels::session_ended).
    app.set_cookie("session_token", &ann.token);
    app.post_form("/session/delete", &[])
        .await
        .assert_redirect_to("/");
    assert_eq!(laptop.closed().await, (4401, Some("revoked".into())));
    assert_eq!(phone.request(json!({ "t": "ping" })).await["t"], "pong");
}

#[tokio::test]
async fn signing_out_everywhere_else_closes_the_other_devices_sockets() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let ann_phone = ann.second_session(&app.state).await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut laptop = Ws::as_guest(addr, &ann).await;
    let mut phone = Ws::as_guest(addr, &ann_phone).await;
    app.set_cookie("session_token", &ann.token);
    app.post_form("/account/sessions/delete-others", &[])
        .await
        .assert_redirect_to(&paths::account());
    assert_eq!(phone.closed().await, (4401, Some("revoked".into())));
    assert_eq!(laptop.request(json!({ "t": "ping" })).await["t"], "pong");
}

/// A shared computer: Bob signs in where Ann's session still was. Ann's session is replaced, and
/// her open sockets close at once instead of at their next re-check.
#[tokio::test]
async fn signing_in_over_a_session_closes_its_sockets() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut anns_tab = Ws::as_guest(addr, &ann).await;
    anns_tab.join("room:lobby", None).await.unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("session_token={}", ann.token)).unwrap(),
    );
    let meta = RequestMeta {
        meta: Meta::default(),
        device: None,
    };
    let bob_user = user::find_self(&app.state.db, bob.id).await.unwrap();
    auth::sign_in(&app.state, &headers, &meta, bob_user, "test")
        .await
        .unwrap();
    assert_eq!(anns_tab.closed().await, (4401, Some("revoked".into())));
}

#[tokio::test]
async fn an_accounts_presence_shows_a_name_never_the_email() {
    let app = TestApp::new().await;
    // The address is ada.lovelace+N@example.com: nothing of it may show, not even the local part
    // turned into a name (people pick addresses like firstname.lastname, or worse).
    let ada = guest(&app.state, "Ada Lovelace").await;
    assert_eq!(ada.name, format!("Member {}", ada.id));
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ada).await;
    ws.join("room:lobby", None).await.unwrap();
    let here = ws.presence("room:lobby").await;
    let text = here.to_string().to_lowercase();
    for leak in ["@", "example.com", "ada", "lovelace"] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }
    assert_eq!(here["here"][ada.key()]["name"], ada.name.as_str());
}

#[tokio::test]
async fn with_accounts_the_guest_pages_send_people_to_sign_in() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await;
    app.get("/live").await.assert_redirect_to("/session/new");
    app.get("/dev/guests/sign-in")
        .await
        .assert_redirect_to("/session/new");
    app.post_form("/dev/guests/sign-in", &[("name", "Intruder")])
        .await
        .assert_redirect_to("/session/new");
    // A signed-in account's session survives the guest sign-out (it never touches guest rows).
    let ann = guest(&app.state, "Ann").await;
    app.set_cookie("session_token", &ann.token);
    app.post_form("/dev/guests/sign-out", &[])
        .await
        .assert_redirect_to("/session/new");
    app.get("/live/lobby")
        .await
        .assert_ok()
        .assert_text(&ann.name)
        .assert_not_contains("/dev/guests/sign-out");
}
