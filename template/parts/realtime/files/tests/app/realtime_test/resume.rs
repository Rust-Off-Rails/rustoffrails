//! Resume after a reconnect: exactly what was missed, in order; a gap when it's too much.

use offrails_host::channels::live_room::REPLAY_LIMIT;
use offrails_host::models::room_message;
use serde_json::json;

use super::client::{Ws, guest, serve};
use crate::factories;
use crate::support::TestApp;

#[tokio::test]
async fn a_reconnect_replays_exactly_what_was_missed() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    a.join("room:lobby", None).await.unwrap();
    let first = a
        .push("room:lobby", "post", json!({ "body": "before" }))
        .await
        .unwrap();
    let seen = first["id"].as_i64().unwrap();

    // Bob was here up to `seen`, then lost his connection; Ann kept talking.
    let mut missed = Vec::new();
    for body in ["one", "two", "three"] {
        missed.push(
            a.push("room:lobby", "post", json!({ "body": body }))
                .await
                .unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }
    let mut b = Ws::as_guest(addr, &bob).await;
    let joined = b.join("room:lobby", Some(seen)).await.unwrap();
    assert!(!joined.gap);
    let seqs: Vec<i64> = joined
        .replay
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, missed, "exactly the missed ones, oldest first");
    let bodies: Vec<&str> = joined
        .replay
        .iter()
        .map(|e| e["data"]["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["one", "two", "three"]);
    assert_eq!(joined.data["seq"], *missed.last().unwrap());
    // Then live traffic continues after the replay.
    a.push("room:lobby", "post", json!({ "body": "four" }))
        .await
        .unwrap();
    assert_eq!(
        b.event("room:lobby", "message").await["data"]["body"],
        "four"
    );
}

#[tokio::test]
async fn a_join_without_a_cursor_replays_nothing() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let lobby = factories::find_room(app.db(), "lobby").await;
    let m = room_message::create(
        app.db(),
        lobby.id,
        ann.id,
        "Ann",
        "old".into(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let joined = ws.join("room:lobby", None).await.unwrap();
    assert!(
        joined.replay.is_empty(),
        "a fresh page rendered the history itself"
    );
    assert_eq!(joined.data["seq"], m.id);
}

#[tokio::test]
async fn too_much_missed_is_a_gap() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let lobby = factories::find_room(app.db(), "lobby").await;
    for i in 0..=REPLAY_LIMIT {
        room_message::create(
            app.db(),
            lobby.id,
            ann.id,
            "Ann",
            format!("m{i}"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let joined = ws.join("room:lobby", Some(0)).await.unwrap();
    assert!(joined.gap, "more than the room replays: reload");
    assert!(joined.replay.is_empty());
    // A cursor from the future (a reset database) is a gap too.
    let mut ws = Ws::as_guest(addr, &ann).await;
    let joined = ws.join("room:lobby", Some(i64::MAX / 2)).await.unwrap();
    assert!(joined.gap);
}

#[tokio::test]
async fn replays_belong_to_their_room() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let lobby = factories::find_room(app.db(), "lobby").await;
    let design = factories::find_room(app.db(), "design").await;
    let now = chrono::Utc::now();
    room_message::create(app.db(), design.id, ann.id, "Ann", "elsewhere".into(), now)
        .await
        .unwrap();
    let here = room_message::create(app.db(), lobby.id, ann.id, "Ann", "here".into(), now)
        .await
        .unwrap();
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ws = Ws::as_guest(addr, &ann).await;
    let joined = ws.join("room:lobby", Some(0)).await.unwrap();
    let ids: Vec<i64> = joined
        .replay
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [here.id]);
}
