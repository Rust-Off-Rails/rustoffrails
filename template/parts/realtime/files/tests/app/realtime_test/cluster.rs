//! More than one node on one database: presence, messages, typing, revocations and
//! re-authorizations cross nodes; a dead node's people leave; a new node learns who's here.
//! On Postgres the nodes talk over LISTEN/NOTIFY; on SQLite over the in-process bus.

use std::time::Duration;

use offrails_host::config::Config;
use offrails_host::kernel::cluster::{Cluster, LocalBus, Timing};
use offrails_host::kernel::hub::Hub;
use offrails_host::models::room;
use offrails_host::state::AppState;
use serde_json::json;

use super::client::{Ws, guest, serve};
use super::people;
use crate::factories;
use crate::support::{TestApp, wait_until};

/// Builds node `name` on the test's database, joined to `bus` (SQLite) or Postgres.
async fn node(app: &TestApp, name: &str, bus: &LocalBus, timing: Timing) -> AppState {
    let mut state = AppState::new(Config::test(), app.db().clone()).expect("state");
    let cluster = if app.on_sqlite() {
        Cluster::local(name, bus.clone())
    } else {
        Cluster::postgres(name, app.db().clone()).expect("postgres")
    };
    let config = (*state.hub.config()).clone();
    state.hub = Hub::with_cluster(cluster.with_timing(timing), config);
    state.hub.start().await.expect("join the bus");
    state
}

fn fast() -> Timing {
    Timing {
        heartbeat: Duration::from_millis(200),
        dead_after: Duration::from_millis(1000),
    }
}

#[tokio::test]
async fn presence_messages_and_typing_cross_nodes() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let a = node(&app, "node-a", &bus, fast()).await;
    let b = node(&app, "node-b", &bus, fast()).await;
    let ann = guest(&a, "Ann").await;
    let bob = guest(&a, "Bob").await;
    let (addr_a, _sa) = serve(a.clone()).await;
    let (addr_b, _sb) = serve(b.clone()).await;

    let mut wa = Ws::as_guest(addr_a, &ann).await;
    wa.join("room:lobby", None).await.unwrap();
    let mut wb = Ws::as_guest(addr_b, &bob).await;
    wb.join("room:lobby", None).await.unwrap();

    // Ann (node A) sees Bob (node B) arrive; Bob's snapshot or a diff shows Ann.
    wa.recv_where("Bob's join", |v| {
        v["t"] == "presence" && v["joins"].get(bob.key()).is_some()
    })
    .await;
    wait_until("node B knows Ann is here", async || {
        b.hub.is_present("room:lobby", &ann.key())
    })
    .await;

    let sent = wa
        .push("room:lobby", "post", json!({ "body": "across nodes" }))
        .await
        .unwrap();
    let ev = wb.event("room:lobby", "message").await;
    assert_eq!(
        (ev["seq"].clone(), ev["data"]["body"].clone()),
        (sent["id"].clone(), json!("across nodes"))
    );

    wb.push("room:lobby", "typing", json!({})).await.unwrap();
    let typing = wa.event("room:lobby", "typing").await;
    assert_eq!(typing["data"]["name"], bob.name.as_str());
}

#[tokio::test]
async fn a_revocation_on_one_node_closes_sockets_on_another() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let a = node(&app, "node-a", &bus, fast()).await;
    let b = node(&app, "node-b", &bus, fast()).await;
    let ann = guest(&a, "Ann").await;
    let (addr_b, _sb) = serve(b.clone()).await;
    let mut ws = Ws::as_guest(addr_b, &ann).await;
    ws.join("room:lobby", None).await.unwrap();
    // Signed out through node A.
    people::end_session(&a.db, ann.session_id).await;
    offrails_host::channels::session_ended(&a, ann.id);
    assert_eq!(ws.closed().await, (4401, Some("revoked".into())));
}

#[tokio::test]
async fn reauthorize_crosses_nodes() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let a = node(&app, "node-a", &bus, fast()).await;
    let b = node(&app, "node-b", &bus, fast()).await;
    let bob = guest(&a, "Bob").await;
    let backstage = factories::find_room(app.db(), "backstage").await;
    room::add_member(app.db(), backstage.id, bob.id, chrono::Utc::now())
        .await
        .unwrap();
    let (addr_b, _sb) = serve(b.clone()).await;
    let mut ws = Ws::as_guest(addr_b, &bob).await;
    ws.join("room:backstage", None).await.unwrap();
    room::remove_member(app.db(), backstage.id, bob.id)
        .await
        .unwrap();
    a.hub.reauthorize("room:backstage");
    let kicked = ws.recv_where("the err", |v| v["t"] == "err").await;
    assert_eq!(
        (kicked["topic"].clone(), kicked["code"].clone()),
        (json!("room:backstage"), json!("forbidden"))
    );
}

#[tokio::test]
async fn a_dead_nodes_people_leave_after_its_heartbeats_stop() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let a = node(&app, "node-a", &bus, fast()).await;
    let b = node(&app, "node-b", &bus, fast()).await;
    let ann = guest(&a, "Ann").await;
    let bob = guest(&a, "Bob").await;
    let (addr_a, _sa) = serve(a.clone()).await;
    let (addr_b, server_b) = serve(b.clone()).await;
    let mut wa = Ws::as_guest(addr_a, &ann).await;
    wa.join("room:lobby", None).await.unwrap();
    let mut wb = Ws::as_guest(addr_b, &bob).await;
    wb.join("room:lobby", None).await.unwrap();
    wait_until("node A sees Bob", async || {
        a.hub.is_present("room:lobby", &bob.key())
    })
    .await;

    // Node B crashes: no Bye, no more heartbeats.
    b.hub.cluster().halt();
    server_b.abort();
    let left = wa
        .recv_where("Bob's leave", |v| {
            v["t"] == "presence" && v["leaves"].get(bob.key()).is_some()
        })
        .await;
    assert_eq!(left["leaves"][bob.key()]["name"], bob.name.as_str());
    assert!(a.hub.is_present("room:lobby", &ann.key()));
    drop(wb);
}

#[tokio::test]
async fn a_clean_shutdown_leaves_at_once() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let slow = Timing {
        heartbeat: Duration::from_millis(200),
        dead_after: Duration::from_secs(600),
    };
    let a = node(&app, "node-a", &bus, slow).await;
    let b = node(&app, "node-b", &bus, slow).await;
    let ann = guest(&a, "Ann").await;
    let bob = guest(&a, "Bob").await;
    let (addr_a, _sa) = serve(a.clone()).await;
    let (addr_b, _sb) = serve(b.clone()).await;
    let mut wa = Ws::as_guest(addr_a, &ann).await;
    wa.join("room:lobby", None).await.unwrap();
    let mut wb = Ws::as_guest(addr_b, &bob).await;
    wb.join("room:lobby", None).await.unwrap();
    wait_until("node A sees Bob", async || {
        a.hub.is_present("room:lobby", &bob.key())
    })
    .await;
    b.hub.close();
    // Bob's socket is told to come back (to any node), and node A drops him without waiting
    // ten minutes for a heartbeat.
    assert_eq!(wb.closed().await, (1012, Some("restart".into())));
    wa.recv_where("Bob's leave", |v| {
        v["t"] == "presence" && v["leaves"].get(bob.key()).is_some()
    })
    .await;
}

#[tokio::test]
async fn a_node_that_starts_later_learns_who_is_here() {
    let app = TestApp::new().await;
    let bus = LocalBus::new();
    let a = node(&app, "node-a", &bus, fast()).await;
    let ann = guest(&a, "Ann").await;
    let bob = guest(&a, "Bob").await;
    let (addr_a, _sa) = serve(a.clone()).await;
    let mut wa = Ws::as_guest(addr_a, &ann).await;
    wa.join("room:lobby", None).await.unwrap();

    let b = node(&app, "node-b", &bus, fast()).await;
    wait_until("node B synced Ann's presence", async || {
        b.hub.is_present("room:lobby", &ann.key())
    })
    .await;
    let (addr_b, _sb) = serve(b.clone()).await;
    let mut wb = Ws::as_guest(addr_b, &bob).await;
    wb.join("room:lobby", None).await.unwrap();
    let here = wb.presence("room:lobby").await;
    assert_eq!(
        here["here"][ann.key()]["name"],
        ann.name.as_str(),
        "two people, two nodes"
    );
    assert_eq!(here["here"].as_object().unwrap().len(), 2);
    assert_eq!(a.hub.cluster().nodes().len(), 2);
}
