//! Who may connect, join and stay: origins, sessions, authorization on join and push, revocation,
//! expiry, caps.

use std::time::Duration;

use axum::http::StatusCode;
use offrails_host::channels::identity;
use offrails_host::config::{AppEnv, Config};
use offrails_host::kernel::channels::SocketConfig;
use offrails_host::kernel::cluster::Cluster;
use offrails_host::kernel::hub::Hub;
use offrails_host::models::room;
use serde_json::json;

use super::client::{ORIGIN, Ws, config_revalidating_every, guest, serve};
use super::people;
use crate::factories;
use crate::support::{TestApp, wait_until};

#[tokio::test]
async fn logged_out_handshakes_are_401() {
    let app = TestApp::new().await;
    let (addr, _server) = serve(app.state.clone()).await;
    assert_eq!(
        Ws::connect(addr, None, Some(ORIGIN)).await.err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        Ws::connect(addr, Some("session_token=not-a-session"), Some(ORIGIN))
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED)
    );
}

#[tokio::test]
async fn a_foreign_or_missing_origin_is_refused_at_the_handshake() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    for origin in [
        Some("https://evil.example"),
        Some("http://localhost:3001"),
        None,
    ] {
        assert_eq!(
            Ws::connect(addr, Some(&ann.cookie()), origin).await.err(),
            Some(StatusCode::FORBIDDEN),
            "origin {origin:?}"
        );
    }
    assert!(
        Ws::connect(addr, Some(&ann.cookie()), Some(ORIGIN))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn production_reads_only_the_host_prefixed_session_cookie() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "http://localhost:3000".into(),
        ..Config::test()
    })
    .await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    // A plain `session_token` (planted by a sibling subdomain) signs nobody in.
    assert_eq!(
        Ws::connect(addr, Some(&ann.cookie()), Some(ORIGIN))
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    let host = format!("__Host-session_token={}", ann.token);
    assert!(Ws::connect(addr, Some(&host), Some(ORIGIN)).await.is_ok());
    assert_eq!(identity::cookie_name(true), "__Host-session_token");
}

#[tokio::test]
async fn joining_someone_elses_topic_is_forbidden() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let err = ws
        .join(&format!("user:{}", bob.id), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "forbidden");
    // The exact id only: no padding, no tricks.
    let err = ws
        .join(&format!("user:0{}", ann.id), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "forbidden");
    ws.join(&format!("user:{}", ann.id), None).await.unwrap();
}

#[tokio::test]
async fn unknown_topics_are_refused_by_default() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    for topic in ["admin:all", "lobby", "room", "<script>", ""] {
        let err = ws.join(topic, None).await.unwrap_err();
        assert_eq!(err.code, "unknown_topic", "topic {topic:?}");
    }
}

#[tokio::test]
async fn a_members_only_room_and_a_missing_room_look_the_same() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let closed = ws.join("room:backstage", None).await.unwrap_err();
    let missing = ws.join("room:no-such-room", None).await.unwrap_err();
    assert_eq!(closed, missing, "topic names reveal nothing");
    assert_eq!(closed.code, "forbidden");

    let backstage = factories::find_room(app.db(), "backstage").await;
    room::add_member(app.db(), backstage.id, ann.id, chrono::Utc::now())
        .await
        .unwrap();
    ws.join("room:backstage", None).await.unwrap();
}

#[tokio::test]
async fn a_push_after_losing_access_is_forbidden_and_leaves() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let backstage = factories::find_room(app.db(), "backstage").await;
    room::add_member(app.db(), backstage.id, ann.id, chrono::Utc::now())
        .await
        .unwrap();
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:backstage", None).await.unwrap();
    room::remove_member(app.db(), backstage.id, ann.id)
        .await
        .unwrap();
    let err = ws
        .push("room:backstage", "post", json!({ "body": "still here?" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "forbidden");
    let err = ws
        .push("room:backstage", "post", json!({ "body": "again" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_joined", "the refused push left the topic");
}

#[tokio::test]
async fn reauthorize_removes_a_member_who_lost_access_at_once() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let backstage = factories::find_room(app.db(), "backstage").await;
    for g in [&ann, &bob] {
        room::add_member(app.db(), backstage.id, g.id, chrono::Utc::now())
            .await
            .unwrap();
    }
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    a.join("room:backstage", None).await.unwrap();
    b.join("room:backstage", None).await.unwrap();
    room::remove_member(app.db(), backstage.id, bob.id)
        .await
        .unwrap();
    app.state.hub.reauthorize("room:backstage");
    let kicked = b
        .recv_where("the err", |v| {
            v["t"] == "err" && v["topic"] == "room:backstage"
        })
        .await;
    assert_eq!(kicked["code"], "forbidden");
    // Ann stays, and sees Bob leave.
    let left = a
        .recv_where("Bob's leave", |v| {
            v["t"] == "presence" && v["leaves"].get(bob.key()).is_some()
        })
        .await;
    assert_eq!(left["leaves"][bob.key()]["name"], bob.name.as_str());
    a.push("room:backstage", "post", json!({ "body": "just us" }))
        .await
        .unwrap();
    assert!(
        b.quiet(300, |v| v["t"] == "event").await,
        "Bob no longer gets the room's events"
    );
}

#[tokio::test]
async fn logout_closes_that_sessions_sockets_only() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let ann_phone = ann.second_session(&app.state).await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut laptop = Ws::as_guest(addr, &ann).await;
    let mut laptop_tab2 = Ws::as_guest(addr, &ann).await;
    let mut phone = Ws::as_guest(addr, &ann_phone).await;
    for ws in [&mut laptop, &mut laptop_tab2, &mut phone] {
        ws.join("room:lobby", None).await.unwrap();
    }
    // Sign out the laptop's session, as the sign-out handler does.
    people::end_session(app.db(), ann.session_id).await;
    offrails_host::channels::session_ended(&app.state, ann.id);

    assert_eq!(laptop.closed().await, (4401, Some("revoked".into())));
    assert_eq!(laptop_tab2.closed().await, (4401, Some("revoked".into())));
    // The phone's session is still live: it stays.
    let pong = phone.request(json!({ "t": "ping" })).await;
    assert_eq!(pong["t"], "pong");
}

#[tokio::test]
async fn signing_out_everywhere_closes_every_socket_of_that_person() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let ann_phone = ann.second_session(&app.state).await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut laptop = Ws::as_guest(addr, &ann).await;
    let mut phone = Ws::as_guest(addr, &ann_phone).await;
    let mut other = Ws::as_guest(addr, &bob).await;
    people::end_all_sessions(app.db(), ann.id).await;
    offrails_host::channels::session_ended(&app.state, ann.id);
    assert_eq!(laptop.closed().await.0, 4401);
    assert_eq!(phone.closed().await.0, 4401);
    assert_eq!(other.request(json!({ "t": "ping" })).await["t"], "pong");
}

#[tokio::test]
async fn an_expired_session_is_closed_by_revalidation() {
    let app = TestApp::with_config(config_revalidating_every(1)).await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    // Idle for longer than the session lifetime, with no revocation signal at all.
    people::idle_session(app.db(), ann.session_id, 31).await;
    assert_eq!(ws.closed().await, (4401, Some("revoked".into())));
}

#[tokio::test]
async fn revalidation_also_removes_topics_the_member_lost() {
    let app = TestApp::with_config(config_revalidating_every(1)).await;
    let ann = guest(&app.state, "Ann").await;
    let backstage = factories::find_room(app.db(), "backstage").await;
    room::add_member(app.db(), backstage.id, ann.id, chrono::Utc::now())
        .await
        .unwrap();
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:backstage", None).await.unwrap();
    room::remove_member(app.db(), backstage.id, ann.id)
        .await
        .unwrap();
    let kicked = ws
        .recv_where("the err", |v| {
            v["t"] == "err" && v["topic"] == "room:backstage"
        })
        .await;
    assert_eq!(kicked["code"], "forbidden");
}

#[tokio::test]
async fn the_connection_cap_refuses_the_next_socket_with_1008() {
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
    assert_eq!(cap, 128, "sized for tabs");
    // WHY: hold cap - 1 slots instead of opening 127 sockets.
    let _held: Vec<_> = (0..cap - 1)
        .map(|_| app.state.hub.register(&member, "test"))
        .collect();
    let (addr, _server) = serve(app.state.clone()).await;
    let mut last = Ws::as_guest(addr, &ann).await;
    last.join("room:lobby", None).await.unwrap();
    let mut over = Ws::as_guest(addr, &ann).await;
    assert_eq!(over.closed().await, (1008, Some("limit".into())));
    // Someone else is unaffected.
    let bob = guest(&app.state, "Bob").await;
    Ws::as_guest(addr, &bob)
        .await
        .join("room:lobby", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn caps_on_topics_and_joins_per_minute_hold() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let mut cfg = SocketConfig::new(vec![ORIGIN.into()]);
    cfg.max_topics = 2;
    cfg.max_joins_per_min = 5;
    let mut state = app.state.clone();
    state.hub = Hub::with_cluster(Cluster::off("caps"), cfg);
    let (addr, _server) = serve(state).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    ws.join("room:design", None).await.unwrap();
    let err = ws
        .join(&format!("user:{}", ann.id), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "limit");
    ws.leave("room:design").await.unwrap();
    // Joins counted so far: lobby, design. Three refused ones count too.
    for _ in 0..3 {
        let _ = ws.join("room:no-such-room", None).await;
    }
    let err = ws.join("room:design", None).await.unwrap_err();
    assert_eq!(err.code, "rate_limited", "5 joins a minute, refused or not");
}

#[tokio::test]
async fn a_silent_client_is_closed_as_idle() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let mut cfg = SocketConfig::new(vec![ORIGIN.into()]);
    cfg.idle_timeout = Duration::from_millis(500);
    cfg.ping_every = Duration::from_secs(60);
    let mut state = app.state.clone();
    state.hub = Hub::with_cluster(Cluster::off("idle"), cfg);
    let (addr, _server) = serve(state).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    assert_eq!(ws.closed().await, (1001, Some("idle".into())));
}

#[tokio::test]
async fn shutdown_says_restart_and_closes_1012() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    app.state.hub.close();
    assert_eq!(ws.closed().await, (1012, Some("restart".into())));
}

#[tokio::test]
async fn a_slow_consumer_is_warned_then_dropped() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    // The client stops reading while the room floods it: the socket's buffers fill, the topic's
    // queue passes 3/4 (`slow`), then overflows (`too_slow`, 1013). It resumes from its cursor.
    let big = json!({ "filler": "x".repeat(3500) });
    for _ in 0..3000 {
        app.state.hub.broadcast("room:lobby", None, "noise", &big);
        tokio::task::yield_now().await;
    }
    let (code, bye) = ws.closed().await;
    assert_eq!((code, bye.as_deref()), (1013, Some("too_slow")));
}

#[tokio::test]
async fn a_client_that_stops_reading_is_dropped() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let mut cfg = SocketConfig::new(vec![ORIGIN.into()]);
    cfg.send_timeout = Duration::from_millis(300);
    let mut state = app.state.clone();
    state.hub = Hub::with_cluster(Cluster::off("stuck"), cfg);
    let (addr, _server) = serve(state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    // The client never reads again; the server keeps sending until its writes stall.
    let big = json!({ "filler": "x".repeat(3500) });
    let flood = {
        let hub = state.hub.clone();
        tokio::spawn(async move {
            for _ in 0..20_000 {
                hub.broadcast("room:lobby", None, "noise", &big);
                tokio::task::yield_now().await;
                if hub.total_conns() == 0 {
                    break;
                }
            }
        })
    };
    wait_until("the stuck connection is dropped", async || {
        state.hub.total_conns() == 0
    })
    .await;
    flood.abort();
    drop(ws);
}

#[tokio::test]
async fn presence_carries_names_never_emails() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    let here = ws.presence("room:lobby").await;
    let text = here.to_string();
    assert!(!text.contains('@'), "presence: {text}");
    let meta = &here["here"][ann.key()];
    let keys: Vec<&String> = meta.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["color", "initials", "name"]);
}

#[tokio::test]
async fn the_frame_log_is_off_outside_development() {
    let app = TestApp::new().await;
    assert!(!app.state.hub.is_logging());
    let dev = TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await;
    assert!(dev.state.hub.is_logging());
    let prod = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    })
    .await;
    assert!(!prod.state.hub.is_logging());
    // Counters still count without text.
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    wait_until("the join is counted", async || {
        app.state
            .hub
            .snapshot()
            .conns
            .iter()
            .any(|c| c.frames_in >= 1)
    })
    .await;
    assert!(
        app.state
            .hub
            .snapshot()
            .conns
            .iter()
            .all(|c| c.log.is_empty())
    );
}
