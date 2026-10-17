//! The wire: frames, replies, typed events, and what closes a connection.

use axum::http::StatusCode;
use offrails_host::models::room_message;
use sea_orm::EntityTrait;
use serde_json::json;

use super::client::{ORIGIN, Ws, guest, serve};
use crate::support::TestApp;

#[tokio::test]
async fn a_join_answers_ok_with_the_cursor_then_presence_here_including_the_joiner() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann Example").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    assert_eq!(ws.protocol.as_deref(), Some("offrails.v1"));

    let joined = ws.join("room:lobby", None).await.unwrap();
    assert_eq!(joined.data["seq"], 0);
    assert!(!joined.gap && joined.replay.is_empty());
    let here = ws.presence("room:lobby").await;
    assert_eq!(here["here"][ann.key()]["name"], ann.name.as_str());
    // "AE" for the guest "Ann Example"; an account's initials come from its own display name.
    assert_eq!(
        here["here"][ann.key()]["initials"],
        offrails_host::models::guest::initials(&ann.name)
    );
    assert_eq!(offrails_host::models::guest::initials("Ann Example"), "AE");
}

#[tokio::test]
async fn refs_are_echoed_and_ping_gets_pong() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.send_json(json!({ "t": "ping", "ref": 42 })).await;
    let pong = ws.next().await.unwrap();
    assert_eq!(pong, json!({ "t": "pong", "ref": 42 }));
    ws.send_json(json!({ "t": "ping", "ref": "abc", "future_field": true }))
        .await;
    assert_eq!(
        ws.next().await.unwrap()["ref"],
        "abc",
        "unknown fields are ignored"
    );
}

#[tokio::test]
async fn a_message_reaches_everyone_in_the_room_with_its_id_as_seq() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    a.join("room:lobby", None).await.unwrap();
    b.join("room:lobby", None).await.unwrap();

    let reply = a
        .push(
            "room:lobby",
            "post",
            json!({ "body": "  hello <b>world</b>  " }),
        )
        .await
        .unwrap();
    let id = reply["id"].as_i64().unwrap();
    for ws in [&mut a, &mut b] {
        let ev = ws.event("room:lobby", "message").await;
        assert_eq!(ev["seq"], id);
        assert_eq!(ev["data"]["id"], id);
        assert_eq!(
            ev["data"]["body"], "hello <b>world</b>",
            "raw text; pages escape it"
        );
        assert_eq!(ev["data"]["author"], ann.name.as_str());
    }
}

#[tokio::test]
async fn a_message_is_stored_before_anyone_sees_it() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    a.join("room:lobby", None).await.unwrap();
    b.join("room:lobby", None).await.unwrap();
    a.send_json(json!({ "t": "push", "ref": "1", "topic": "room:lobby", "event": "post", "data": { "body": "durable" } }))
        .await;
    // The moment Bob sees it, the row exists.
    let ev = b.event("room:lobby", "message").await;
    let row = room_message::Entity::find_by_id(ev["seq"].as_i64().unwrap())
        .one(app.db())
        .await
        .unwrap()
        .expect("stored before it was broadcast");
    assert_eq!(row.body, "durable");
    assert_eq!(row.author_id, ann.id);
}

#[tokio::test]
async fn a_push_cannot_post_as_someone_else() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    a.join("room:lobby", None).await.unwrap();
    // A client-sent author is refused outright, never used.
    let err = a
        .push(
            "room:lobby",
            "post",
            json!({ "body": "hi", "author_id": bob.id, "author": "Bob" }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_event");
    assert!(
        room_message::Entity::find()
            .all(app.db())
            .await
            .unwrap()
            .is_empty()
    );
    // And the author of an accepted message is the session's person.
    a.push("room:lobby", "post", json!({ "body": "mine" }))
        .await
        .unwrap();
    let rows = room_message::Entity::find().all(app.db()).await.unwrap();
    assert_eq!(
        (rows[0].author_id, rows[0].author_name.as_str()),
        (ann.id, ann.name.as_str())
    );
}

#[tokio::test]
async fn an_undeclared_event_is_bad_event_and_invalid_data_says_why() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    let err = ws
        .push("room:lobby", "drop_tables", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_event");
    let err = ws
        .push("room:lobby", "post", json!({ "body": "   " }))
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.message.as_str()),
        ("invalid", "Body can't be blank")
    );
    let err = ws
        .push("room:lobby", "post", json!({ "body": "x".repeat(1001) }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid");
}

#[tokio::test]
async fn pushes_and_leaves_need_a_join_and_a_join_happens_once() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let err = ws
        .push("room:lobby", "post", json!({ "body": "hi" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_joined");
    assert_eq!(ws.leave("room:lobby").await.unwrap_err().code, "not_joined");
    ws.join("room:lobby", None).await.unwrap();
    assert_eq!(
        ws.join("room:lobby", None).await.unwrap_err().code,
        "already_joined"
    );
    ws.leave("room:lobby").await.unwrap();
    assert_eq!(
        ws.push("room:lobby", "post", json!({ "body": "hi" }))
            .await
            .unwrap_err()
            .code,
        "not_joined"
    );
}

#[tokio::test]
async fn the_subprotocol_is_required() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    // None offered: the socket opens, says why, closes 1002 (browsers hide HTTP refusals).
    let mut ws = Ws::connect_with(addr, Some(&ann.cookie()), Some(ORIGIN), &[])
        .await
        .unwrap();
    assert_eq!(ws.closed().await, (1002, Some("protocol".into())));
    // Only other subprotocols offered: the server selects none, so the client can't connect.
    assert!(
        Ws::connect_with(addr, Some(&ann.cookie()), Some(ORIGIN), &["graphql-ws"])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn garbage_unknown_types_and_arrays_close_with_1002() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    for bad in [
        "not json",
        r#"{"t":"nope"}"#,
        r#"["join","room:lobby"]"#,
        r#"{"t":"join"}"#,
    ] {
        let mut ws = Ws::as_guest(addr, &ann).await;
        ws.send_text(bad).await;
        assert_eq!(ws.closed().await, (1002, Some("protocol".into())), "{bad}");
    }
}

#[tokio::test]
async fn binary_frames_close_with_1003() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.send_binary(vec![1, 2, 3]).await;
    assert_eq!(ws.closed().await, (1003, Some("protocol".into())));
}

#[tokio::test]
async fn an_oversized_frame_closes_the_socket() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    let big = json!({ "t": "push", "ref": "1", "topic": "room:lobby", "event": "post", "data": { "body": "x".repeat(5000) } });
    ws.send_json(big).await;
    let (code, _bye) = ws.closed().await;
    assert!(
        matches!(code, 1009 | 1006 | 1002),
        "an oversized frame ends the connection (close {code})"
    );
    assert!(
        room_message::Entity::find()
            .all(app.db())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn flooding_closes_the_socket_with_1008() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    for i in 0..101 {
        ws.send_json(json!({ "t": "ping", "ref": i })).await;
    }
    let (code, bye) = ws.closed().await;
    assert_eq!((code, bye.as_deref()), (1008, Some("rate_limited")));
}

#[tokio::test]
async fn the_per_room_post_limit_answers_rate_limited_without_closing() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    for i in 0..10 {
        ws.push("room:lobby", "post", json!({ "body": format!("m{i}") }))
            .await
            .unwrap();
    }
    let err = ws
        .push("room:lobby", "post", json!({ "body": "one too many" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "rate_limited");
    // Still connected.
    ws.send_json(json!({ "t": "ping", "ref": "still" })).await;
    let pong = ws.recv_where("pong", |v| v["t"] == "pong").await;
    assert_eq!(pong["ref"], "still");
}

#[tokio::test]
async fn html_in_messages_is_delivered_raw_and_rendered_escaped() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    ws.push(
        "room:lobby",
        "post",
        json!({ "body": "<script>alert(1)</script>" }),
    )
    .await
    .unwrap();
    app.set_cookie("session_token", &ann.token);
    let page = app.get("/live/lobby").await;
    page.assert_ok()
        .assert_not_contains("<script>alert(1)</script>")
        .assert_text("<script>alert(1)</script>");
    assert_eq!(page.status, StatusCode::OK);
}
