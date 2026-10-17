//! The fallbacks: SSE + POST and long polling speak the same protocol as the socket, with the
//! same guarantees, and relays can't be used by anyone but their owner.

use axum::http::StatusCode;
use offrails_host::channels::identity;
use offrails_host::config::Config;
use serde_json::json;
use tower::ServiceExt;

use super::client::{Relay, Ws, browser_request, guest, router_of, serve};
use super::people;
use crate::support::{TestApp, wait_until};

#[tokio::test]
async fn sse_and_post_speak_the_protocol_and_talk_to_websocket_clients() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &bob).await;
    ws.join("room:lobby", None).await.unwrap();

    let mut sse = Relay::sse(&app.state, &ann).await;
    assert_eq!(
        sse.post(json!({ "t": "join", "ref": "1", "topic": "room:lobby" }))
            .await,
        StatusCode::ACCEPTED
    );
    let ok = sse.recv_where("the join's ok", |v| v["t"] == "ok").await;
    assert_eq!(ok["ref"], "1");
    let here = sse.recv_where("here", |v| v["t"] == "presence").await;
    assert_eq!(here["here"].as_object().unwrap().len(), 2);

    sse.post(json!({ "t": "push", "ref": "2", "topic": "room:lobby", "event": "post", "data": { "body": "over sse" } }))
        .await;
    assert_eq!(
        ws.event("room:lobby", "message").await["data"]["body"],
        "over sse"
    );
    ws.push("room:lobby", "post", json!({ "body": "over a socket" }))
        .await
        .unwrap();
    let ev = sse
        .recv_where("Bob's message", |v| v["data"]["body"] == "over a socket")
        .await;
    assert_eq!(ev["data"]["author"], bob.name.as_str());
}

#[tokio::test]
async fn long_polling_speaks_the_protocol_too() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &bob).await;
    ws.join("room:lobby", None).await.unwrap();

    let mut poll = Relay::longpoll(&app.state, &ann).await;
    poll.post(json!({ "t": "join", "ref": "1", "topic": "room:lobby" }))
        .await;
    poll.recv_where("ok", |v| v["t"] == "ok" && v["ref"] == "1")
        .await;
    poll.post(json!({ "t": "push", "ref": "2", "topic": "room:lobby", "event": "post", "data": { "body": "over polling" } }))
        .await;
    let reply = poll.recv_where("the push's ok", |v| v["ref"] == "2").await;
    assert!(reply["data"]["id"].as_i64().is_some());
    assert_eq!(
        ws.event("room:lobby", "message").await["data"]["body"],
        "over polling"
    );
}

#[tokio::test]
async fn relays_need_their_owner_session_and_token() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let ann_phone = ann.second_session(&app.state).await;
    let bob = guest(&app.state, "Bob").await;
    for kind in ["sse", "poll"] {
        let relay = match kind {
            "sse" => Relay::sse(&app.state, &ann).await,
            _ => Relay::longpoll(&app.state, &ann).await,
        };
        let ping = json!({ "t": "ping", "ref": "x" }).to_string();
        assert_eq!(
            relay.post_as(&relay.cookie, &relay.token, &ping).await,
            StatusCode::ACCEPTED
        );
        let wrong = "A".repeat(relay.token.len());
        assert_eq!(
            relay.post_as(&relay.cookie, &wrong, &ping).await,
            StatusCode::FORBIDDEN,
            "{kind}: wrong token"
        );
        assert_eq!(
            relay.post_as(&relay.cookie, "", &ping).await,
            StatusCode::FORBIDDEN,
            "{kind}: no token"
        );
        assert_eq!(
            relay.post_as(&bob.cookie(), &relay.token, &ping).await,
            StatusCode::FORBIDDEN,
            "{kind}: another person"
        );
        assert_eq!(
            relay
                .post_as(&ann_phone.cookie(), &relay.token, &ping)
                .await,
            StatusCode::FORBIDDEN,
            "{kind}: the same person's other session"
        );
        if kind == "poll" {
            assert_eq!(
                relay.poll_as(&bob.cookie(), &relay.token).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                relay.poll_as(&relay.cookie, &wrong).await.0,
                StatusCode::FORBIDDEN
            );
        }
    }
    // An id that isn't a relay looks the same as someone else's.
    let req = {
        let mut r = browser_request(
            "POST",
            "/socket/poll/999999",
            &ann.cookie(),
            Some("{}".into()),
        );
        r.headers_mut()
            .insert("x-offrails-token", "x".parse().unwrap());
        r
    };
    let res = router_of(&app.state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_site_requests_are_refused_on_every_transport() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let router = router_of(&app.state);
    let poll = Relay::longpoll(&app.state, &ann).await;
    for (method, path) in [
        ("GET", "/socket/sse".to_owned()),
        ("POST", "/socket/poll".to_owned()),
        ("GET", format!("/socket/poll/{}", poll.conn)),
        ("POST", format!("/socket/poll/{}", poll.conn)),
        ("POST", "/socket/token".to_owned()),
    ] {
        // A current browser (both headers), an older one (Origin only), and a cross-site GET
        // that carries no Origin at all.
        let mut variants = vec![
            (Some("cross-site"), Some("https://evil.example")),
            (None, Some("https://evil.example")),
        ];
        if method == "GET" {
            variants.push((Some("cross-site"), None));
        }
        for (site, origin) in variants {
            let mut req = browser_request(
                method,
                &path,
                &ann.cookie(),
                (method == "POST").then(|| "{}".into()),
            );
            let h = req.headers_mut();
            h.remove("sec-fetch-site");
            h.remove("origin");
            if let Some(site) = site {
                h.insert("sec-fetch-site", site.parse().unwrap());
            }
            if let Some(origin) = origin {
                h.insert("origin", origin.parse().unwrap());
            }
            h.insert("x-offrails-token", poll.token.parse().unwrap());
            let res = router.clone().oneshot(req).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{method} {path} ({site:?}, {origin:?})"
            );
        }
    }
}

#[tokio::test]
async fn an_oversized_relay_post_is_413() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let relay = Relay::longpoll(&app.state, &ann).await;
    let big = json!({ "t": "push", "ref": "1", "topic": "room:lobby", "event": "post", "data": { "body": "x".repeat(5000) } }).to_string();
    assert_eq!(
        relay.post_as(&relay.cookie, &relay.token, &big).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn the_relay_cap_answers_429() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let member = identity::member_for(&identity::Signed {
        person: identity::Person {
            id: ann.id,
            name: ann.name.clone(),
        },
        session_id: ann.session_id,
    });
    let cap = app.state.hub.config().max_conns_per_user;
    let _held: Vec<_> = (0..cap)
        .map(|_| app.state.hub.register(&member, "test"))
        .collect();
    let router = router_of(&app.state);
    for (method, path) in [("GET", "/socket/sse"), ("POST", "/socket/poll")] {
        let res = router
            .clone()
            .oneshot(browser_request(method, path, &ann.cookie(), None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "{path}");
    }
}

#[tokio::test]
async fn logout_ends_relays_with_bye() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let mut sse = Relay::sse(&app.state, &ann).await;
    let poll = Relay::longpoll(&app.state, &ann).await;
    assert_eq!(app.state.hub.conns_of(&ann.key()), 2);
    people::end_session(app.db(), ann.session_id).await;
    offrails_host::channels::session_ended(&app.state, ann.id);
    // The stream says why, then ends.
    let bye = sse.recv_where("bye", |v| v["t"] == "bye").await;
    assert_eq!(bye["code"], "revoked");
    assert!(sse.next().await.is_none(), "the stream ended");
    // A poller's next request carries a cookie that signs nobody in any more: a bare 401, and
    // its connection is gone too.
    assert_eq!(
        poll.poll_as(&poll.cookie, &poll.token).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        poll.post_as(&poll.cookie, &poll.token, "{}").await,
        StatusCode::UNAUTHORIZED
    );
    wait_until("both relays ended", async || {
        app.state.hub.conns_of(&ann.key()) == 0
    })
    .await;
}

#[tokio::test]
async fn transports_can_be_switched_off() {
    let mut config = Config::test();
    config.realtime.transports = vec!["sse".into(), "longpoll".into()];
    let app = TestApp::with_config(config).await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    // Like a proxy that blocks WebSockets: the client moves to the next transport.
    assert_eq!(
        Ws::connect(addr, Some(&ann.cookie()), Some(super::client::ORIGIN))
            .await
            .err(),
        Some(StatusCode::NOT_FOUND)
    );
    Relay::sse(&app.state, &ann).await;
}

#[tokio::test]
async fn relay_responses_are_no_store() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let res = router_of(&app.state)
        .oneshot(browser_request("POST", "/socket/poll", &ann.cookie(), None))
        .await
        .unwrap();
    assert_eq!(res.headers()["cache-control"], "no-store");
    let (status, headers) = app
        .request_head("/socket/sse", &[("cookie", &ann.cookie())])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["content-type"], "text/event-stream");
    assert_eq!(headers["x-content-type-options"], "nosniff");
}
