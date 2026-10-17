//! Socket tokens: for clients that can't send the session cookie (another origin, a native app).

use axum::http::StatusCode;
use offrails_host::config::Config;
use offrails_host::kernel::socket_token;
use serde_json::Value;

use super::client::{Guest, ORIGIN, Ws, guest, serve};
use super::people;
use crate::support::TestApp;

async fn mint(app: &TestApp, g: &Guest) -> String {
    app.set_cookie("session_token", &g.token);
    let res = app.post_form("/socket/token", &[]).await;
    res.assert_ok();
    let v: Value = res.json();
    assert_eq!(v["expires_in"], 60);
    app.clear_cookies();
    v["token"].as_str().unwrap().to_owned()
}

fn offered(token: &str) -> String {
    format!("offrails.token.{token}")
}

#[tokio::test]
async fn a_socket_token_works_once() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let token = mint(&app, &ann).await;
    let (addr, _server) = serve(app.state.clone()).await;
    // No cookie, no Origin: a native client.
    let mut ws = Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&token)])
        .await
        .expect("the token signs the socket in");
    assert_eq!(
        ws.protocol.as_deref(),
        Some("offrails.v1"),
        "the token is never echoed"
    );
    ws.join(&format!("user:{}", ann.id), None).await.unwrap();
    // A replay, on any node, is refused.
    assert_eq!(
        Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&token)])
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED)
    );
}

#[tokio::test]
async fn a_forged_or_altered_socket_token_is_refused() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let token = mint(&app, &ann).await;
    let (addr, _server) = serve(app.state.clone()).await;
    // Ann's token, edited to name Bob's person and session: the signature no longer matches.
    let altered = token.replacen(
        &format!("v1.{}.{}.", ann.id, ann.session_id),
        &format!("v1.{}.{}.", bob.id, bob.session_id),
        1,
    );
    assert_ne!(altered, token);
    let other_secret = "o".repeat(32).into_bytes();
    let forged = socket_token::mint(&other_secret, bob.id, bob.session_id, chrono::Utc::now());
    for bad in [
        altered.as_str(),
        forged.as_str(),
        "v1.1.1.1.x.y",
        "nonsense",
    ] {
        assert_eq!(
            Ws::connect_with(addr, None, None, &["offrails.v1", &offered(bad)])
                .await
                .err(),
            Some(StatusCode::UNAUTHORIZED),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn an_expired_socket_token_is_refused() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let secret = app.state.config.realtime.secret.clone().unwrap();
    let old = socket_token::mint(
        secret.as_bytes(),
        ann.id,
        ann.session_id,
        chrono::Utc::now() - chrono::Duration::seconds(61),
    );
    let (addr, _server) = serve(app.state.clone()).await;
    assert_eq!(
        Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&old)])
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED)
    );
}

#[tokio::test]
async fn a_token_from_an_ended_session_is_refused_and_a_token_socket_closes_when_it_ends() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let t1 = mint(&app, &ann).await;
    let t2 = mint(&app, &ann).await;
    let mut ws = Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&t1)])
        .await
        .unwrap();
    ws.join("room:lobby", None).await.unwrap();
    people::end_session(app.db(), ann.session_id).await;
    offrails_host::channels::session_ended(&app.state, ann.id);
    assert_eq!(ws.closed().await, (4401, Some("revoked".into())));
    assert_eq!(
        Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&t2)])
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED)
    );
}

#[tokio::test]
async fn token_sockets_follow_their_own_origin_rule_and_ignore_cookies() {
    let mut config = Config::test();
    config.realtime.token_origins = vec!["https://partner.example.com".into()];
    let app = TestApp::with_config(config).await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;

    let t = mint(&app, &bob).await;
    assert_eq!(
        Ws::connect_with(
            addr,
            None,
            Some("https://evil.example"),
            &["offrails.v1", &offered(&t)]
        )
        .await
        .err(),
        Some(StatusCode::FORBIDDEN),
        "an origin outside both lists"
    );
    // That refusal happened before the token was looked at, so it's still unused.
    let mut ws = Ws::connect_with(
        addr,
        Some(&ann.cookie()),
        Some("https://partner.example.com"),
        &["offrails.v1", &offered(&t)],
    )
    .await
    .expect("an allowed token origin");
    // The token decides who this is, not Ann's cookie.
    ws.join(&format!("user:{}", bob.id), None).await.unwrap();
    assert_eq!(
        ws.join(&format!("user:{}", ann.id), None)
            .await
            .unwrap_err()
            .code,
        "forbidden"
    );
    // And the partner origin with a cookie alone is still a cross-site handshake.
    assert_eq!(
        Ws::connect(
            addr,
            Some(&ann.cookie()),
            Some("https://partner.example.com")
        )
        .await
        .err(),
        Some(StatusCode::FORBIDDEN)
    );
    assert!(
        Ws::connect_with(
            addr,
            None,
            Some(ORIGIN),
            &["offrails.v1", &offered(&mint(&app, &ann).await)]
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn minting_needs_a_session_a_same_origin_post_and_a_secret() {
    let app = TestApp::new().await;
    app.post_form("/socket/token", &[])
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let ann = guest(&app.state, "Ann").await;
    app.set_cookie("session_token", &ann.token);
    app.post_form_with_headers("/socket/token", &[], &[("sec-fetch-site", "cross-site")])
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let res = app.post_form("/socket/token", &[]).await;
    res.assert_ok();
    assert_eq!(res.headers["cache-control"], "no-store");
    let body: Value = res.json();
    assert!(
        body["protocol"]
            .as_str()
            .unwrap()
            .starts_with("offrails.token.v1.")
    );

    let mut config = Config::test();
    config.realtime.secret = None;
    let off = TestApp::with_config(config).await;
    let bob = guest(&off.state, "Bob").await;
    off.set_cookie("session_token", &bob.token);
    off.post_form("/socket/token", &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let forged = socket_token::mint(
        &"t".repeat(32).into_bytes(),
        bob.id,
        bob.session_id,
        chrono::Utc::now(),
    );
    let (addr, _server) = serve(off.state.clone()).await;
    assert_eq!(
        Ws::connect_with(addr, None, None, &["offrails.v1", &offered(&forged)])
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED),
        "without a secret, tokens are off"
    );
}
