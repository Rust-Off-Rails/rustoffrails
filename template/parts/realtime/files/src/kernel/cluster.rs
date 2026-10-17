//! More than one node: the database is the bus (no Redis, no node-to-node HTTP).
//!
//! GUIDE: .offrails/guides/channels.md#more-than-one-server-kernelclusterrs
//!
//! Every node is equal. A node publishes what its own connections do (broadcasts, presence
//! refs, revocations, re-authorizations) and applies what the others publish. Durable data never
//! travels on the bus: messages are rows, and a client that missed something resumes from its
//! cursor out of the database.
//!
//! - **Postgres** (`APP_CLUSTER=true`): `pg_notify('offrails_realtime', json)`, received by one
//!   `PgListener` per node. Payloads stay under `MAX_NOTIFY` (frames are capped at 4 KiB).
//! - **In-process** (`LocalBus`): the same messages over a tokio channel, for hubs that share a
//!   process (SQLite apps, and the multi-node tests on SQLite).
//! - **Presence without CRDTs:** each node is the only writer of its own refs, and publishes
//!   them in order from one task. A new node says `Hello`; every node answers with a full `Sync`
//!   of its refs, which replaces what the others knew about it. Heartbeats every 2 s; a node
//!   silent for 6 s is dead and its refs leave. A clean shutdown says `Bye`.
//! - **Lost messages** (the LISTEN connection dropped, the local bus lagged): the node asks for
//!   a fresh `Sync` and closes its sockets with 1012, so clients resume from their cursors.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::kernel::hub::{NodeInfo, Payload};

/// The `NOTIFY` channel.
pub const CHANNEL: &str = "offrails_realtime";
/// Postgres refuses `NOTIFY` payloads of 8000 bytes or more.
pub const MAX_NOTIFY: usize = 7900;
/// What a `Sync` chunk may hold before the next one starts.
const SYNC_CHUNK: usize = 6000;

/// One bus message, from `node`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub node: String,
    pub msg: BusMsg,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum BusMsg {
    Fanout {
        topic: String,
        seq: Option<i64>,
        event: String,
        data: Payload,
    },
    Track {
        topic: String,
        conn: u64,
        key: String,
        meta: Payload,
    },
    Untrack {
        topic: String,
        conn: u64,
    },
    Revoke {
        key: String,
    },
    Reauthorize {
        topic: String,
    },
    /// "I'm new (or I lost messages): send me your refs."
    Hello,
    /// One chunk of a node's complete list of refs; `last` closes generation `generation`.
    Sync {
        generation: u64,
        last: bool,
        refs: Vec<SyncRef>,
    },
    Heartbeat,
    Bye,
}

/// One presence ref in a `Sync`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncRef {
    pub topic: String,
    pub conn: u64,
    pub key: String,
    pub meta: Payload,
}

/// What the bus needs from the hub.
pub trait ClusterPeer: Send + Sync + 'static {
    fn apply(&self, env: Envelope);
    fn replace_node(&self, node: &str, refs: Vec<SyncRef>);
    fn node_gone(&self, node: &str);
    fn local_refs(&self) -> Vec<SyncRef>;
    fn lost_messages(&self);
}

/// An in-process bus: hubs created with clones of one `LocalBus` hear each other.
#[derive(Clone)]
pub struct LocalBus {
    tx: broadcast::Sender<Arc<Envelope>>,
}

impl LocalBus {
    pub fn new() -> Self {
        Self {
            tx: broadcast::channel(4096).0,
        }
    }
}

impl Default for LocalBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
enum Bus {
    Off,
    Local(LocalBus),
    Postgres(DatabaseConnection),
}

/// Heartbeat cadence and the silence after which a node is dead.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub heartbeat: Duration,
    pub dead_after: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(2),
            dead_after: Duration::from_secs(6),
        }
    }
}

pub struct Cluster {
    node: String,
    bus: Bus,
    timing: Timing,
    peers: Arc<Mutex<HashMap<String, Instant>>>,
    out: Mutex<Option<mpsc::UnboundedSender<BusMsg>>>,
    stop: watch::Sender<bool>,
    generation: Arc<AtomicU64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `<host>:<pid>:<random>`: unique per process, so a restarted node is a new node.
pub fn node_name(configured: Option<&str>) -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "node".to_owned());
    let base = configured
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{host}:{}", std::process::id()));
    format!("{base}:{:04x}", rand::random::<u16>())
}

impl Cluster {
    /// No bus: this node alone.
    pub fn off(node: impl Into<String>) -> Self {
        Self::new(node.into(), Bus::Off)
    }

    /// The in-process bus.
    pub fn local(node: impl Into<String>, bus: LocalBus) -> Self {
        Self::new(node.into(), Bus::Local(bus))
    }

    /// `LISTEN`/`NOTIFY` on this Postgres database.
    pub fn postgres(node: impl Into<String>, db: DatabaseConnection) -> anyhow::Result<Self> {
        anyhow::ensure!(
            db.get_database_backend() == DbBackend::Postgres,
            "APP_CLUSTER=true needs Postgres (DATABASE_URL is not postgres://)"
        );
        Ok(Self::new(node.into(), Bus::Postgres(db)))
    }

    fn new(node: String, bus: Bus) -> Self {
        Self {
            node,
            bus,
            timing: Timing::default(),
            peers: Arc::default(),
            out: Mutex::new(None),
            stop: watch::channel(false).0,
            generation: Arc::default(),
        }
    }

    /// Faster heartbeats for tests.
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    /// "off", "local" or "postgres".
    pub fn kind(&self) -> &'static str {
        match self.bus {
            Bus::Off => "off",
            Bus::Local(_) => "local",
            Bus::Postgres(_) => "postgres",
        }
    }

    /// Queues a message for the other nodes. Nothing before `start`, and nothing without a bus.
    pub fn publish(&self, msg: BusMsg) {
        if let Some(out) = lock(&self.out).as_ref() {
            let _ = out.send(msg);
        }
    }

    /// This node and every node heard from recently.
    pub fn nodes(&self) -> Vec<NodeInfo> {
        let mut nodes = vec![NodeInfo {
            node: self.node.clone(),
            seen_secs_ago: 0,
            this: true,
        }];
        let mut others: Vec<NodeInfo> = lock(&self.peers)
            .iter()
            .map(|(node, seen)| NodeInfo {
                node: node.clone(),
                seen_secs_ago: seen.elapsed().as_secs(),
                this: false,
            })
            .collect();
        others.sort_by(|a, b| a.node.cmp(&b.node));
        nodes.extend(others);
        nodes
    }

    /// Graceful shutdown: `Bye`, then stop the tasks.
    pub fn leave(&self) {
        self.publish(BusMsg::Bye);
        let _ = self.stop.send(true);
    }

    /// Tests: die without telling anyone, like a crashed node. The others notice by the missing
    /// heartbeats.
    pub fn halt(&self) {
        *lock(&self.out) = None;
        let _ = self.stop.send(true);
    }

    /// Starts the publisher, the receiver and the heartbeat. Idempotent; a no-op without a bus.
    pub async fn start(&self, peer: Arc<dyn ClusterPeer>) -> anyhow::Result<()> {
        if matches!(self.bus, Bus::Off) || lock(&self.out).is_some() {
            return Ok(());
        }
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Incoming>();
        match &self.bus {
            Bus::Off => {}
            Bus::Local(bus) => spawn_local_receiver(bus.tx.subscribe(), in_tx, self.stopped()),
            Bus::Postgres(db) => {
                // WHY: `LISTEN` is set up asynchronously, and a NOTIFY sent before it is in place
                // is lost: wait for it before saying Hello.
                let (ready_tx, ready_rx) = oneshot::channel();
                spawn_pg_listener(db.clone(), in_tx, ready_tx, self.stopped());
                tokio::time::timeout(Duration::from_secs(10), ready_rx)
                    .await
                    .map_err(|_| anyhow::anyhow!("LISTEN {CHANNEL} timed out"))?
                    .map_err(|_| anyhow::anyhow!("LISTEN {CHANNEL} failed"))?;
            }
        }
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        spawn_publisher(self.node.clone(), self.bus.clone(), out_rx);
        *lock(&self.out) = Some(out_tx);
        self.spawn_receiver(in_rx, peer.clone());
        self.spawn_heartbeat(peer);
        self.publish(BusMsg::Hello);
        Ok(())
    }

    fn stopped(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.stop.subscribe();
        async move {
            let _ = rx.wait_for(|s| *s).await;
        }
    }

    fn spawn_receiver(
        &self,
        mut rx: mpsc::UnboundedReceiver<Incoming>,
        peer: Arc<dyn ClusterPeer>,
    ) {
        let me = self.node.clone();
        let peers = self.peers.clone();
        let out = lock(&self.out).clone();
        let generation = self.generation.clone();
        let stop = self.stopped();
        tokio::spawn(async move {
            tokio::pin!(stop);
            let publish = |msg: BusMsg| {
                if let Some(out) = &out {
                    let _ = out.send(msg);
                }
            };
            // node → (generation, refs gathered so far)
            let mut syncs: HashMap<String, (u64, Vec<SyncRef>)> = HashMap::new();
            loop {
                let incoming = tokio::select! {
                    () = &mut stop => break,
                    i = rx.recv() => match i { Some(i) => i, None => break },
                };
                let env = match incoming {
                    Incoming::Lost => {
                        peer.lost_messages();
                        publish(BusMsg::Hello);
                        continue;
                    }
                    Incoming::Message(env) => env,
                };
                if env.node == me {
                    continue;
                }
                let new_peer = lock(&peers)
                    .insert(env.node.clone(), Instant::now())
                    .is_none();
                if new_peer && !matches!(env.msg, BusMsg::Hello | BusMsg::Bye) {
                    // Heard from a node we never synced with: ask everyone for their refs.
                    publish(BusMsg::Hello);
                }
                match env.msg {
                    BusMsg::Hello => {
                        let n = generation.fetch_add(1, Ordering::Relaxed);
                        for msg in sync_messages(n, peer.local_refs()) {
                            publish(msg);
                        }
                    }
                    BusMsg::Sync {
                        generation,
                        last,
                        refs,
                    } => {
                        let entry = syncs
                            .entry(env.node.clone())
                            .or_insert_with(|| (generation, Vec::new()));
                        if entry.0 != generation {
                            *entry = (generation, Vec::new());
                        }
                        entry.1.extend(refs);
                        if last && let Some((_, all)) = syncs.remove(&env.node) {
                            peer.replace_node(&env.node, all);
                        }
                    }
                    BusMsg::Heartbeat => {}
                    BusMsg::Bye => {
                        lock(&peers).remove(&env.node);
                        syncs.remove(&env.node);
                        peer.apply(env);
                    }
                    _ => peer.apply(env),
                }
            }
        });
    }

    fn spawn_heartbeat(&self, peer: Arc<dyn ClusterPeer>) {
        let timing = self.timing;
        let peers = self.peers.clone();
        let out = lock(&self.out).clone();
        let stop = self.stopped();
        tokio::spawn(async move {
            tokio::pin!(stop);
            let mut tick = tokio::time::interval(timing.heartbeat);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = &mut stop => break,
                    _ = tick.tick() => {}
                }
                if let Some(out) = &out {
                    let _ = out.send(BusMsg::Heartbeat);
                }
                let dead: Vec<String> = {
                    let mut peers = lock(&peers);
                    let dead: Vec<String> = peers
                        .iter()
                        .filter(|(_, seen)| seen.elapsed() > timing.dead_after)
                        .map(|(node, _)| node.clone())
                        .collect();
                    for node in &dead {
                        peers.remove(node);
                    }
                    dead
                };
                for node in dead {
                    tracing::warn!(%node, "realtime node silent; dropping its presence");
                    peer.node_gone(&node);
                }
            }
        });
    }
}

/// A node's complete list of refs as `Sync` chunks that each fit a `NOTIFY`.
fn sync_messages(generation: u64, refs: Vec<SyncRef>) -> Vec<BusMsg> {
    let mut chunks: Vec<Vec<SyncRef>> = vec![Vec::new()];
    let mut size = 0;
    for r in refs {
        let len = serde_json::to_string(&r).map_or(0, |s| s.len());
        if size + len > SYNC_CHUNK && chunks.last().is_some_and(|c| !c.is_empty()) {
            chunks.push(Vec::new());
            size = 0;
        }
        size += len;
        if let Some(current) = chunks.last_mut() {
            current.push(r);
        }
    }
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, refs)| BusMsg::Sync {
            generation,
            last: i + 1 == n,
            refs,
        })
        .collect()
}

enum Incoming {
    Message(Envelope),
    /// Messages may have been lost before this point.
    Lost,
}

fn spawn_local_receiver(
    mut rx: broadcast::Receiver<Arc<Envelope>>,
    tx: mpsc::UnboundedSender<Incoming>,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::spawn(async move {
        tokio::pin!(stop);
        loop {
            let got = tokio::select! {
                () = &mut stop => break,
                got = rx.recv() => got,
            };
            let sent = match got {
                Ok(env) => tx.send(Incoming::Message((*env).clone())),
                Err(broadcast::error::RecvError::Lagged(_)) => tx.send(Incoming::Lost),
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if sent.is_err() {
                break;
            }
        }
    });
}

/// WHY: a dedicated task owns the `PgListener`: a `recv` inside a `select!` with other branches
/// would be dropped mid-read whenever another branch wins.
fn spawn_pg_listener(
    db: DatabaseConnection,
    tx: mpsc::UnboundedSender<Incoming>,
    ready: oneshot::Sender<()>,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::spawn(async move {
        use sea_orm::sqlx::postgres::PgListener;
        let pool = db.get_postgres_connection_pool().clone();
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "realtime cluster: LISTEN connection failed");
                return;
            }
        };
        if let Err(e) = listener.listen(CHANNEL).await {
            tracing::error!(error = %e, "realtime cluster: LISTEN failed");
            return;
        }
        let _ = ready.send(());
        tokio::pin!(stop);
        loop {
            let got = tokio::select! {
                () = &mut stop => break,
                got = listener.try_recv() => got,
            };
            let sent = match got {
                Ok(Some(n)) => match serde_json::from_str::<Envelope>(n.payload()) {
                    Ok(env) => tx.send(Incoming::Message(env)),
                    Err(e) => {
                        tracing::warn!(error = %e, "realtime cluster: unreadable message");
                        Ok(())
                    }
                },
                // The connection dropped; the listener reconnects on the next call, and what was
                // sent meanwhile is gone.
                Ok(None) => tx.send(Incoming::Lost),
                Err(e) => {
                    tracing::warn!(error = %e, "realtime cluster: listener error");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    tx.send(Incoming::Lost)
                }
            };
            if sent.is_err() {
                break;
            }
        }
    });
}

/// One task publishes, in order: each node's messages arrive in the order it sent them, which is
/// what makes presence correct without CRDTs (a ref's `Untrack` never overtakes its `Track`).
fn spawn_publisher(node: String, bus: Bus, mut rx: mpsc::UnboundedReceiver<BusMsg>) {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let bye = matches!(msg, BusMsg::Bye);
            let env = Envelope {
                node: node.clone(),
                msg,
            };
            match &bus {
                Bus::Off => {}
                Bus::Local(local) => {
                    let _ = local.tx.send(Arc::new(env));
                }
                Bus::Postgres(db) => {
                    if let Err(e) = notify(db, &env).await {
                        tracing::warn!(error = %e, "realtime cluster: NOTIFY failed");
                    }
                }
            }
            if bye {
                break;
            }
        }
    });
}

async fn notify(db: &DatabaseConnection, env: &Envelope) -> anyhow::Result<()> {
    let payload = serde_json::to_string(env)?;
    anyhow::ensure!(
        payload.len() <= MAX_NOTIFY,
        "a {}-byte cluster message is over NOTIFY's limit; dropped",
        payload.len()
    );
    // Raw SQL in the kernel (guides/database.md): NOTIFY has no builder. Channel and payload are
    // bound values, never formatted into the statement.
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_notify($1, $2)",
        [CHANNEL.into(), payload.into()],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sync_lists_are_cut_into_chunks_that_fit_a_notify() {
        let refs: Vec<SyncRef> = (0..400)
            .map(|i| SyncRef {
                topic: format!("room:{i}"),
                conn: i,
                key: format!("{i}"),
                meta: json!({ "name": "Someone with a long display name", "initials": "SL" }),
            })
            .collect();
        let msgs = sync_messages(1, refs.clone());
        assert!(msgs.len() > 1);
        let mut back = Vec::new();
        for (i, msg) in msgs.iter().enumerate() {
            let env = Envelope {
                node: "a-node-name-of-some-length:12345:abcd".into(),
                msg: msg.clone(),
            };
            assert!(serde_json::to_string(&env).unwrap().len() <= MAX_NOTIFY);
            let BusMsg::Sync { last, refs, .. } = msg else {
                panic!("not a sync")
            };
            assert_eq!(*last, i + 1 == msgs.len());
            back.extend(refs.clone());
        }
        assert_eq!(back, refs);
        let empty = sync_messages(2, Vec::new());
        assert!(matches!(&empty[..], [BusMsg::Sync { last: true, refs, .. }] if refs.is_empty()));
    }

    #[test]
    fn node_names_carry_the_configured_name() {
        assert!(node_name(Some("web-1")).starts_with("web-1:"));
        assert!(node_name(None).contains(&format!(":{}:", std::process::id())));
    }
}
