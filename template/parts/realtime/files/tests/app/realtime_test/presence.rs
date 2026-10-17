//! Who's here: people, not tabs; snapshots before diffs; typing to the others only; waves.

use serde_json::json;

use super::client::{Ws, guest, serve};
use crate::support::TestApp;

#[tokio::test]
async fn two_tabs_are_one_person_until_the_last_one_leaves() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut watcher = Ws::as_guest(addr, &bob).await;
    watcher.join("room:lobby", None).await.unwrap();
    watcher.presence("room:lobby").await; // here: Bob

    let mut tab1 = Ws::as_guest(addr, &ann).await;
    tab1.join("room:lobby", None).await.unwrap();
    // (Bob's own join diff may come first: diffs follow the snapshot and are idempotent.)
    let joined = watcher
        .recv_where("Ann's join", |v| {
            v["t"] == "presence" && v["joins"].get(ann.key()).is_some()
        })
        .await;
    assert_eq!(joined["joins"][ann.key()]["name"], ann.name.as_str());

    let mut tab2 = Ws::as_guest(addr, &ann).await;
    tab2.join("room:lobby", None).await.unwrap();
    let here = tab2.presence("room:lobby").await;
    assert_eq!(
        here["here"].as_object().unwrap().len(),
        2,
        "two people, three tabs"
    );
    let about_ann = |v: &serde_json::Value| {
        v["t"] == "presence"
            && (v["joins"].get(ann.key()).is_some() || v["leaves"].get(ann.key()).is_some())
    };
    assert!(
        watcher.quiet(300, about_ann).await,
        "a second tab is not a new person"
    );

    drop(tab2);
    assert!(
        watcher.quiet(300, about_ann).await,
        "Ann is still here through her first tab"
    );
    tab1.leave("room:lobby").await.unwrap();
    let left = watcher
        .recv_where("Ann's leave", |v| {
            v["t"] == "presence" && v["leaves"].get(ann.key()).is_some()
        })
        .await;
    assert_eq!(left["leaves"][ann.key()]["name"], ann.name.as_str());
    assert!(!app.state.hub.is_present("room:lobby", &ann.key()));
}

#[tokio::test]
async fn a_closed_socket_leaves_presence() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut watcher = Ws::as_guest(addr, &bob).await;
    watcher.join("room:lobby", None).await.unwrap();
    let mut ws = Ws::as_guest(addr, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    watcher
        .recv_where("Ann's join", |v| v["joins"].get(ann.key()).is_some())
        .await;
    drop(ws);
    let left = watcher
        .recv_where("Ann's leave", |v| v["leaves"].get(ann.key()).is_some())
        .await;
    assert_eq!(left["topic"], "room:lobby");
}

#[tokio::test]
async fn presence_is_per_topic() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    a.join("room:lobby", None).await.unwrap();
    b.join("room:design", None).await.unwrap();
    let here = b.presence("room:design").await;
    assert!(here["here"].get(ann.key()).is_none());
    assert!(
        a.quiet(300, |v| v["t"] == "presence"
            && v["joins"].get(bob.key()).is_some())
            .await
    );
}

#[tokio::test]
async fn typing_reaches_the_others_but_none_of_the_typists_tabs() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut ann1 = Ws::as_guest(addr, &ann).await;
    let mut ann2 = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    for ws in [&mut ann1, &mut ann2, &mut b] {
        ws.join("room:lobby", None).await.unwrap();
    }
    ann1.push("room:lobby", "typing", json!({})).await.unwrap();
    let typing = b.event("room:lobby", "typing").await;
    assert_eq!(
        typing["data"],
        json!({ "key": ann.key(), "name": ann.name })
    );
    assert!(
        typing.get("seq").is_none(),
        "ephemeral: no cursor, never replayed"
    );
    assert!(
        ann1.quiet(300, |v| v["event"] == "typing").await,
        "broadcast_from skips the sender"
    );
    assert!(
        ann2.quiet(300, |v| v["event"] == "typing").await,
        "handle_out skips her other tabs"
    );
}

#[tokio::test]
async fn a_wave_reaches_only_its_target_on_their_private_topic() {
    let app = TestApp::new().await;
    let ann = guest(&app.state, "Ann").await;
    let bob = guest(&app.state, "Bob").await;
    let cy = guest(&app.state, "Cy").await;
    let (addr, _server) = serve(app.state.clone()).await;
    let mut a = Ws::as_guest(addr, &ann).await;
    let mut b = Ws::as_guest(addr, &bob).await;
    let mut c = Ws::as_guest(addr, &cy).await;
    a.join("room:lobby", None).await.unwrap();
    b.join("room:lobby", None).await.unwrap();
    b.join(&format!("user:{}", bob.id), None).await.unwrap();
    c.join("room:lobby", None).await.unwrap();
    c.join(&format!("user:{}", cy.id), None).await.unwrap();

    a.push("room:lobby", "wave", json!({ "to": bob.id }))
        .await
        .unwrap();
    let waved = b.event(&format!("user:{}", bob.id), "waved").await;
    assert_eq!(waved["data"], json!({ "from": ann.name, "room": "lobby" }));
    assert!(c.quiet(300, |v| v["event"] == "waved").await);

    // Not in the room, or yourself: refused (no pinging arbitrary ids).
    let err = a
        .push("room:lobby", "wave", json!({ "to": 999_999 }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid");
    let err = a
        .push("room:lobby", "wave", json!({ "to": ann.id }))
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid");
}
