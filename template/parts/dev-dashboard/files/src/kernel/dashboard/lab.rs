//! The load lab (development only): fake data, a traffic generator against the app's own address,
//! and scenarios that light up one panel each (a slow query, an N+1, a panic, a memory spike, a
//! slow endpoint, an error burst). `app bench` runs the same generator from a terminal.
//!
//! The scenarios hit real routes under `/dev/lab/*`, so what the dashboard shows is what the
//! middleware, the SQL callback and the tracing layer actually saw.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use askama::Template;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, extract::Extension};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::Deserialize;
use tower::ServiceExt;
use tracing::Instrument;

use super::hist::Hist;
use super::http::{IGNORED, UNMATCHED, route_of};
use super::{HUB, lock};
use crate::error::AppError;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui::{self, Chrome};
use crate::routes::ROUTES;
use crate::state::AppState;

/// The generator's hard caps.
pub const MAX_RPS: u32 = 200; // APP: raise only on a machine that can take it
pub const MAX_SECS: u32 = 60;
pub const MAX_INFLIGHT: usize = 32;
/// Fake records per seed, at most.
pub const MAX_SEED: u64 = 10_000;
/// The memory spike: how much, and for how long.
pub const SPIKE_MB: usize = 256; // APP: smaller on a small machine
pub const SPIKE_HOLD: Duration = Duration::from_secs(8);
/// Rows the slow query counts through (a recursive CTE: ~0.3 s on SQLite, ~0.5 s on Postgres).
pub const SLOW_QUERY_ROWS: i64 = 1_500_000;
/// Paths the generator never requests: streams never finish.
const STREAMING: &[&str] = &["/events", "/stream"];

// ---------------------------------------------------------------------------------------------
// Fake data: orders and their items
// ---------------------------------------------------------------------------------------------

pub mod order {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "lab_orders")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub reference: String,
        pub customer: String,
        pub status: String,
        pub total_cents: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod order_item {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "lab_order_items")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub order_id: i64,
        pub sku: String,
        pub quantity: i32,
        pub price_cents: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

const STATUSES: [&str; 4] = ["paid", "shipped", "pending", "refunded"];
const SKUS: [(&str, i64); 6] = [
    ("SKU-MUG", 1_400),
    ("SKU-TEE", 2_200),
    ("SKU-CAP", 1_800),
    ("SKU-PIN", 300),
    ("SKU-BAG", 4_500),
    ("SKU-SOCKS", 900),
];

/// APP: the seed hook. Writes `n` fake records (here: orders with 1 to 4 items each) and returns
/// how many it wrote. Point it at your own models to load-test your own pages; keep the values
/// obviously fake (example.com addresses).
pub async fn seed(db: &DatabaseConnection, n: u64) -> Result<u64, DbErr> {
    let n = n.min(MAX_SEED);
    let txn = crate::kernel::db::begin(db).await?;
    let start = order::Entity::find().count(&txn).await?;
    for i in 0..n {
        let k = start + i + 1;
        let items = rand::random_range(1..=4_usize);
        let mut total = 0;
        let lines: Vec<(&str, i32, i64)> = (0..items)
            .map(|_| {
                let (sku, price) = SKUS[rand::random_range(0..SKUS.len())];
                let qty = rand::random_range(1..=3);
                total += price * i64::from(qty);
                (sku, qty, price)
            })
            .collect();
        let o = order::ActiveModel {
            reference: Set(format!("ORD-{:05}", 10_000 + k)),
            customer: Set(format!("customer{}@example.com", k % 97)),
            status: Set(STATUSES[(k % 4) as usize].to_owned()),
            total_cents: Set(total),
            created_at: Set(Utc::now() - chrono::Duration::minutes(rand::random_range(0..10_000))),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        order_item::Entity::insert_many(lines.into_iter().map(|(sku, qty, price)| {
            order_item::ActiveModel {
                order_id: Set(o.id),
                sku: Set(sku.to_owned()),
                quantity: Set(qty),
                price_cents: Set(price),
                ..Default::default()
            }
        }))
        .exec(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(n)
}

pub async fn count(db: &DatabaseConnection) -> Result<u64, DbErr> {
    order::Entity::find().count(db).await
}

/// Removes every fake record.
pub async fn clear(db: &DatabaseConnection) -> Result<u64, DbErr> {
    order_item::Entity::delete_many().exec(db).await?;
    Ok(order::Entity::delete_many().exec(db).await?.rows_affected)
}

// ---------------------------------------------------------------------------------------------
// The demo routes the scenarios hit (`/dev/lab/*`, development only)
// ---------------------------------------------------------------------------------------------

pub fn demo_router() -> Router<AppState> {
    Router::new()
        .route("/dev/lab/orders", get(orders))
        .route("/dev/lab/orders/n-plus-one", get(orders_n_plus_one))
        .route("/dev/lab/slow-query", get(slow_query))
        .route("/dev/lab/slow", get(slow))
        .route("/dev/lab/panic", get(panic))
        .route("/dev/lab/error", get(error))
        .layer(ui::no_store())
}

pub struct OrderRow {
    pub reference: String,
    pub customer: String,
    pub status: String,
    pub items: usize,
    pub total: String,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/lab_orders.html")]
struct OrdersPage {
    chrome: Chrome,
    title: &'static str,
    note: &'static str,
    rows: Vec<OrderRow>,
}

fn money(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn render_orders(
    state: &AppState,
    title: &'static str,
    note: &'static str,
    orders: Vec<order::Model>,
    items: impl Fn(i64) -> usize,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Response {
    let _render =
        tracing::info_span!("render", template = "kernel/dashboard/lab_orders.html").entered();
    ui::page(OrdersPage {
        chrome: Chrome::new(
            title,
            "dashboard",
            state.assets.assets(),
            true,
            flash.and_then(|f| f.0.0),
            theme,
        )
        .stylesheet(state.assets.path("kernel/dashboard.css")),
        title,
        note,
        rows: orders
            .iter()
            .map(|o| OrderRow {
                reference: o.reference.clone(),
                customer: o.customer.clone(),
                status: o.status.clone(),
                items: items(o.id),
                total: money(o.total_cents),
            })
            .collect(),
    })
}

/// `GET /dev/lab/orders`: 20 orders and their items in two queries (the right way).
async fn orders(
    State(state): State<AppState>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    orders_body(state, theme, flash)
        .instrument(tracing::info_span!("handler", handler = "lab::orders"))
        .await
}

async fn orders_body(
    state: AppState,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    let (orders, items) = async {
        let orders = order::Entity::find()
            .order_by_desc(order::Column::Id)
            .limit(20)
            .all(&state.db)
            .await?;
        let ids: Vec<i64> = orders.iter().map(|o| o.id).collect();
        let items = order_item::Entity::find()
            .filter(order_item::Column::OrderId.is_in(ids))
            .all(&state.db)
            .await?;
        Ok::<_, DbErr>((orders, items))
    }
    .instrument(tracing::info_span!("load_orders"))
    .await?;
    tracing::info!(job = "lab.receipt_digest", "job enqueued (simulated)");
    Ok(render_orders(
        &state,
        "Lab: orders",
        "Two queries: the orders, then every item at once.",
        orders,
        |id| items.iter().filter(|i| i.order_id == id).count(),
        flash,
        theme,
    ))
}

/// `GET /dev/lab/orders/n-plus-one`: the same page, one query per order (the N+1 the dashboard flags).
async fn orders_n_plus_one(
    State(state): State<AppState>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    orders_n_plus_one_body(state, theme, flash)
        .instrument(tracing::info_span!(
            "handler",
            handler = "lab::orders_n_plus_one"
        ))
        .await
}

async fn orders_n_plus_one_body(
    state: AppState,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    let (orders, counts) = async {
        let orders = order::Entity::find()
            .order_by_desc(order::Column::Id)
            .limit(20)
            .all(&state.db)
            .await?;
        let mut counts = Vec::with_capacity(orders.len());
        for o in &orders {
            let items = order_item::Entity::find()
                .filter(order_item::Column::OrderId.eq(o.id))
                .all(&state.db)
                .await?;
            counts.push((o.id, items.len()));
        }
        Ok::<_, DbErr>((orders, counts))
    }
    .instrument(tracing::info_span!("load_orders", n_plus_one = true))
    .await?;
    Ok(render_orders(
        &state,
        "Lab: orders (N+1)",
        "One query per order: the dashboard flags this page.",
        orders,
        |id| counts.iter().find(|c| c.0 == id).map_or(0, |c| c.1),
        flash,
        theme,
    ))
}

/// `GET /dev/lab/slow-query`: a recursive CTE that counts to `SLOW_QUERY_ROWS`, on either backend.
async fn slow_query(State(state): State<AppState>) -> Result<Response, AppError> {
    let backend = state.db.get_database_backend();
    let sql = match backend {
        DbBackend::Postgres => {
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < $1) SELECT COUNT(*) AS n FROM c"
        }
        _ => {
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < ?) SELECT COUNT(*) AS n FROM c"
        }
    };
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            sql,
            [SLOW_QUERY_ROWS.into()],
        ))
        .instrument(tracing::info_span!("count_rows"))
        .await?;
    let n: i64 = row.and_then(|r| r.try_get("", "n").ok()).unwrap_or(0);
    Ok(format!("lab: counted {n} rows the slow way\n").into_response())
}

/// `GET /dev/lab/slow`: a slow endpoint with a visible shape: a simulated upstream call, a query,
/// a render.
async fn slow(State(state): State<AppState>) -> Result<Response, AppError> {
    slow_body(state)
        .instrument(tracing::info_span!("handler", handler = "lab::slow"))
        .await
}

async fn slow_body(state: AppState) -> Result<Response, AppError> {
    async {
        tokio::time::sleep(Duration::from_millis(rand::random_range(350..550))).await;
    }
    .instrument(tracing::info_span!(
        "upstream_call",
        service = "inventory (simulated)"
    ))
    .await;
    let orders = count(&state.db).await?;
    let body = {
        let _render = tracing::info_span!("render").entered();
        std::thread::sleep(Duration::from_millis(40));
        format!("lab: slow endpoint, {orders} orders\n")
    };
    Ok(body.into_response())
}

/// `GET /dev/lab/panic`: the panic layer turns it into a 500 the dashboard shows.
async fn panic() -> Response {
    panic!("lab: a deliberate panic, caught by the panic layer")
}

#[derive(Deserialize)]
struct ErrorQuery {
    status: Option<u16>,
}

/// `GET /dev/lab/error?status=503`: a deliberate error (400, 404, 409, 422, 429, 500, 502, 503).
async fn error(Query(q): Query<ErrorQuery>) -> Response {
    let code = q
        .status
        .filter(|c| [400, 404, 409, 422, 429, 500, 502, 503].contains(c))
        .unwrap_or(500);
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if code >= 500 {
        tracing::error!(status = code, "lab: a deliberate server error");
    } else {
        tracing::warn!(status = code, "lab: a deliberate client error");
    }
    (status, format!("lab: deliberate {code}\n")).into_response()
}

// ---------------------------------------------------------------------------------------------
// The traffic generator
// ---------------------------------------------------------------------------------------------

/// What to send.
#[derive(Clone, Debug)]
pub struct Plan {
    pub paths: Vec<String>,
    pub rps: u32,
    pub secs: u32,
}

/// Only GET routes of this app, never a stream, never the dashboard itself.
// SECURITY: a path, never a URL: the host is always this app's own address, so the lab can't be
// pointed at another machine (test: traffic_only_targets_this_app).
pub fn allowed_path(path: &str) -> bool {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 200
        || path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\' || c == '#')
    {
        return false;
    }
    let bare = path.split('?').next().unwrap_or(path);
    let route = route_of("GET", bare);
    route != UNMATCHED
        && ROUTES.iter().any(|(m, p, _, _)| *m == "GET" && *p == route)
        && !IGNORED.iter().any(|i| {
            bare == *i
                || bare.starts_with(&format!("{}/", i.trim_end_matches('/')))
                || bare.starts_with(&format!("{i}."))
        })
        && !STREAMING.iter().any(|s| bare.ends_with(s))
}

/// A checked plan, or what's wrong with it.
pub fn plan(paths: &[String], rps: u32, secs: u32) -> Result<Plan, String> {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return Err("Choose at least one route.".into());
    }
    if let Some(bad) = paths.iter().find(|p| !allowed_path(p)) {
        return Err(format!(
            "{bad} isn't a GET route of this app the lab may request."
        ));
    }
    // INVARIANT: the caps hold whatever the form says (test: traffic_only_targets_this_app).
    if rps == 0 || rps > MAX_RPS {
        return Err(format!("Requests per second: 1 to {MAX_RPS}."));
    }
    if secs == 0 || secs > MAX_SECS {
        return Err(format!("Duration: 1 to {MAX_SECS} seconds."));
    }
    Ok(Plan { paths, rps, secs })
}

/// GET routes the lab offers by default (no parameters, not a stream).
pub fn suggested_paths() -> Vec<&'static str> {
    ROUTES
        .iter()
        .filter(|(m, p, _, _)| *m == "GET" && !p.contains('{') && allowed_path(p))
        .map(|(_, p, _, _)| *p)
        .collect()
}

/// Where the requests go.
pub enum Target {
    /// `http://127.0.0.1:3000`: over the loopback, like a real client.
    Address(String),
    /// Through the router without a socket (tests, and a process that isn't serving).
    InProcess(Router),
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::Address(a) => a.clone(),
            Target::InProcess(_) => "in process (no listener)".to_owned(),
        }
    }
}

/// One generator run, as it goes.
pub struct Run {
    pub label: String,
    pub plan: Plan,
    pub target: String,
    pub at: DateTime<Utc>,
    pub started: Instant,
    pub sent: AtomicU64,
    pub ok: AtomicU64,
    pub failed: AtomicU64,
    pub skipped: AtomicU64,
    pub stop: AtomicBool,
    pub finished: AtomicBool,
    pub latency: Mutex<Hist>,
}

impl Run {
    pub fn new(label: &str, plan: Plan, target: &Target) -> Self {
        Self {
            label: label.to_owned(),
            plan,
            target: target.describe(),
            at: Utc::now(),
            started: Instant::now(),
            sent: AtomicU64::new(0),
            ok: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            latency: Mutex::new(Hist::default()),
        }
    }

    pub fn total(&self) -> u64 {
        u64::from(self.plan.rps) * u64::from(self.plan.secs)
    }

    pub fn done(&self) -> u64 {
        self.ok.load(Ordering::Relaxed) + self.failed.load(Ordering::Relaxed)
    }

    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    pub fn percent(&self) -> u64 {
        if !self.running() {
            return 100;
        }
        (self.sent.load(Ordering::Relaxed) * 100)
            .checked_div(self.total())
            .unwrap_or(0)
            .min(100)
    }
}

#[derive(Default)]
pub struct LabState {
    pub run: Mutex<Option<Arc<Run>>>,
    pub spiking: AtomicBool,
    pub spike_until: Mutex<Option<DateTime<Utc>>>,
}

impl LabState {
    pub fn current(&self) -> Option<Arc<Run>> {
        lock(&self.run).clone()
    }

    pub fn busy(&self) -> bool {
        self.current().is_some_and(|r| r.running())
    }
}

fn client(base: &str) -> Result<reqwest::Client, reqwest::Error> {
    // WHY: the lab talks to this very process on the loopback; with direct TLS its certificate
    // names localhost, not 127.0.0.1. Any other host never gets this client.
    let loopback_tls = base.starts_with("https://127.0.0.1:");
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("offrails-load-lab")
        .danger_accept_invalid_certs(loopback_tls)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Sends the plan's requests at its rate (round-robin over its paths), at most `MAX_INFLIGHT` at
/// a time; a request that would exceed that is skipped and counted, never queued.
pub async fn run(target: Target, run: Arc<Run>) {
    let plan = run.plan.clone();
    let http = match &target {
        Target::Address(base) => client(base).ok(),
        Target::InProcess(_) => None,
    };
    let target = Arc::new(target);
    let gate = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT));
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / f64::from(plan.rps)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for i in 0..run.total() {
        if run.stop.load(Ordering::Relaxed) {
            break;
        }
        tick.tick().await;
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            run.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        run.sent.fetch_add(1, Ordering::Relaxed);
        let path = plan.paths[(i as usize) % plan.paths.len()].clone();
        let (run, target, http) = (run.clone(), target.clone(), http.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let ok = send(&target, http.as_ref(), &path).await;
            lock(&run.latency).record(started.elapsed().as_micros() as u64);
            if ok {
                run.ok.fetch_add(1, Ordering::Relaxed);
            } else {
                run.failed.fetch_add(1, Ordering::Relaxed);
            }
            drop(permit);
        });
    }
    // Wait for the stragglers.
    let _ = gate.acquire_many(MAX_INFLIGHT as u32).await;
    run.finished.store(true, Ordering::Relaxed);
}

/// One GET; `true` for a status below 400.
async fn send(target: &Target, http: Option<&reqwest::Client>, path: &str) -> bool {
    match (target, http) {
        (Target::Address(base), Some(http)) => match http.get(format!("{base}{path}")).send().await
        {
            Ok(res) => {
                let ok = res.status().as_u16() < 400;
                let _ = res.bytes().await;
                ok
            }
            Err(_) => false,
        },
        (Target::InProcess(router), _) => {
            let Ok(req) = Request::get(path).body(Body::empty()) else {
                return false;
            };
            match router.clone().oneshot(req).await {
                Ok(res) => {
                    let ok = res.status().as_u16() < 400;
                    let _ = axum::body::to_bytes(res.into_body(), 8 << 20).await;
                    ok
                }
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// Starts a run in the background, unless one is going.
pub fn start(state: &AppState, label: &str, plan: Plan) -> Result<Arc<Run>, String> {
    let mut slot = lock(&HUB.lab.run);
    if slot.as_ref().is_some_and(|r| r.running()) {
        return Err("A run is in progress; stop it first.".into());
    }
    let target = match HUB.serving.get() {
        Some(addr) => Target::Address(addr.clone()),
        None => match crate::routes::router(state.clone()) {
            Ok(r) => Target::InProcess(r),
            Err(e) => return Err(format!("no router: {e}")),
        },
    };
    let r = Arc::new(Run::new(label, plan, &target));
    *slot = Some(r.clone());
    lock(&HUB.series).annotate(super::series::Annotation {
        t: Utc::now().timestamp(),
        kind: "lab",
        label: format!(
            "load lab: {label}, {} req/s for {} s",
            r.plan.rps, r.plan.secs
        ),
        demo: false,
    });
    tokio::spawn(run(target, r.clone()));
    Ok(r)
}

/// Asks the current run to stop.
pub fn stop() -> bool {
    match HUB.lab.current() {
        Some(r) if r.running() => {
            r.stop.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

/// Holds `mb` MiB for `hold`, then frees it: a bump on the memory chart, then the drop.
pub fn memory_spike(mb: usize, hold: Duration) -> bool {
    if HUB.lab.spiking.swap(true, Ordering::SeqCst) {
        return false;
    }
    *lock(&HUB.lab.spike_until) =
        Some(Utc::now() + chrono::Duration::from_std(hold).unwrap_or_default());
    tokio::spawn(async move {
        let block = tokio::task::spawn_blocking(move || {
            let mut v = vec![0_u8; mb << 20];
            // Touch every page so it is resident, not just reserved.
            for i in (0..v.len()).step_by(4096) {
                v[i] = 1;
            }
            std::hint::black_box(v)
        })
        .await;
        tokio::time::sleep(hold).await;
        drop(block);
        *lock(&HUB.lab.spike_until) = None;
        HUB.lab.spiking.store(false, Ordering::SeqCst);
    });
    true
}

// ---------------------------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    SlowQuery,
    NPlusOne,
    Panic,
    MemorySpike,
    SlowEndpoint,
    ErrorBurst,
}

impl Scenario {
    pub const ALL: [Scenario; 6] = [
        Scenario::NPlusOne,
        Scenario::SlowQuery,
        Scenario::SlowEndpoint,
        Scenario::ErrorBurst,
        Scenario::Panic,
        Scenario::MemorySpike,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.key() == s)
    }

    pub fn key(self) -> &'static str {
        match self {
            Scenario::SlowQuery => "slow-query",
            Scenario::NPlusOne => "n-plus-one",
            Scenario::Panic => "panic",
            Scenario::MemorySpike => "memory-spike",
            Scenario::SlowEndpoint => "slow-endpoint",
            Scenario::ErrorBurst => "error-burst",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scenario::SlowQuery => "Slow query",
            Scenario::NPlusOne => "N+1",
            Scenario::Panic => "Panic",
            Scenario::MemorySpike => "Memory spike",
            Scenario::SlowEndpoint => "Slow endpoint",
            Scenario::ErrorBurst => "Error burst",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Scenario::SlowQuery => "Three requests to a page whose query counts 1.5 million rows.",
            Scenario::NPlusOne => {
                "Five requests to an orders page that loads items one order at a time."
            }
            Scenario::Panic => "One request whose handler panics; the panic layer answers 500.",
            Scenario::MemorySpike => "Holds 256 MiB for 8 seconds, then frees it.",
            Scenario::SlowEndpoint => "Four requests to an endpoint with a slow upstream call.",
            Scenario::ErrorBurst => "100 requests over 5 seconds answering 404, 422, 500 and 503.",
        }
    }

    /// The page that lights up.
    pub fn panel(self) -> &'static str {
        match self {
            Scenario::SlowQuery => "/dev/dashboard/sql?sort=max",
            Scenario::NPlusOne => "/dev/dashboard/sql#n-plus-one",
            Scenario::Panic => "/dev/dashboard/logs?level=error",
            Scenario::MemorySpike => "/dev/dashboard/charts?range=5m#chart-memory",
            Scenario::SlowEndpoint => "/dev/dashboard/traces?notable=1",
            Scenario::ErrorBurst => "/dev/dashboard/requests?sort=errors",
        }
    }

    /// The requests it sends (none for the memory spike).
    pub fn plan(self) -> Option<Plan> {
        let p = |paths: &[&str], rps, secs| Plan {
            paths: paths.iter().map(|s| (*s).to_owned()).collect(),
            rps,
            secs,
        };
        match self {
            Scenario::SlowQuery => Some(p(&["/dev/lab/slow-query"], 1, 3)),
            Scenario::NPlusOne => Some(p(&["/dev/lab/orders/n-plus-one"], 1, 5)),
            Scenario::Panic => Some(p(&["/dev/lab/panic"], 1, 1)),
            Scenario::SlowEndpoint => Some(p(&["/dev/lab/slow"], 1, 4)),
            Scenario::ErrorBurst => Some(p(
                &[
                    "/dev/lab/error?status=404",
                    "/dev/lab/error?status=500",
                    "/dev/lab/error?status=422",
                    "/dev/lab/error?status=503",
                ],
                20,
                5,
            )),
            Scenario::MemorySpike => None,
        }
    }
}

/// Runs a scenario in the background; the message for the flash.
pub async fn scenario(state: &AppState, s: Scenario) -> Result<String, String> {
    if s == Scenario::NPlusOne && count(&state.db).await.unwrap_or(0) < 20 {
        seed(&state.db, 50).await.map_err(|e| e.to_string())?;
    }
    match s.plan() {
        Some(plan) => {
            start(state, s.label(), plan)?;
            Ok(format!("{} running: watch this page light up.", s.label()))
        }
        None => {
            if memory_spike(SPIKE_MB, SPIKE_HOLD) {
                Ok(format!(
                    "Holding {SPIKE_MB} MiB for {} s: watch Memory.",
                    SPIKE_HOLD.as_secs()
                ))
            } else {
                Err("A memory spike is already running.".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_round_trip_and_stay_under_the_caps() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::parse(s.key()), Some(s));
            if let Some(p) = s.plan() {
                assert!(p.rps <= MAX_RPS && p.secs <= MAX_SECS);
                assert!(p.paths.iter().all(|path| allowed_path(path)), "{s:?}");
            }
        }
    }
}
