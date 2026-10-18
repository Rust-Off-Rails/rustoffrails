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
use crate::routes::{ROUTES, paths};
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
        .route("/dev/lab/checkout", get(checkout))
        .route(
            "/dev/lab/upstream/{name}/{action}",
            get(upstream).post(upstream),
        )
        .route("/dev/lab/upstream/{name}/{action}/{id}", get(upstream_item))
        .route("/dev/lab/sign-in", get(sign_in_form).post(sign_in))
        .route("/dev/lab/ux", get(ux))
        .route("/dev/lab/ux/widget", get(ux_widget))
        .route("/dev/lab/ux/slow", get(ux_slow))
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
    let (orders, counts) = load_orders_one_by_one(&state.db)
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

/// Orders, and each order's item count.
type OrdersAndCounts = (Vec<order::Model>, Vec<(i64, usize)>);

/// The orders, then each order's items in a query of its own (the N+1). Instrumented the way the
/// README shows ("Instrumenting your own methods"): the waterfall reads
/// `load_orders_one_by_one → recent_orders → sql`, then `load_items ×20 → sql`, folded into one row.
// GUIDE: .offrails/guides/errors-and-logging.md#logging
#[tracing::instrument(level = "debug", skip_all)]
async fn load_orders_one_by_one(db: &DatabaseConnection) -> Result<OrdersAndCounts, DbErr> {
    let orders = recent_orders(db).await?;
    let mut counts = Vec::with_capacity(orders.len());
    for o in &orders {
        counts.push((o.id, load_items(db, o.id).await?.len()));
    }
    Ok((orders, counts))
}

#[tracing::instrument(level = "debug", skip_all)]
async fn recent_orders(db: &DatabaseConnection) -> Result<Vec<order::Model>, DbErr> {
    order::Entity::find()
        .order_by_desc(order::Column::Id)
        .limit(20)
        .all(db)
        .await
}

// The order id is a field (it shows in the side panel); never a secret or personal data there.
#[tracing::instrument(level = "debug", skip_all, fields(order_id))]
async fn load_items(
    db: &DatabaseConnection,
    order_id: i64,
) -> Result<Vec<order_item::Model>, DbErr> {
    tracing::Span::current().record("order_id", order_id);
    order_item::Entity::find()
        .filter(order_item::Column::OrderId.eq(order_id))
        .all(db)
        .await
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
    /// The simulated APIs' behavior (the Upstreams control panel).
    pub sims: Mutex<Option<[UpSim; 4]>>,
    /// The last `traceparent` a simulated API received (proof of propagation).
    pub last_traceparent: Mutex<Option<String>>,
    /// The attack simulator is running.
    pub attacking: AtomicBool,
    /// The lab sign-in form's throttles: per account and per address, (failures, window start).
    pub sign_in: Mutex<std::collections::HashMap<String, (u32, i64)>>,
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
    let mark = super::series::Annotation {
        t: Utc::now().timestamp(),
        kind: "lab",
        label: format!(
            "load lab: {label}, {} req/s for {} s",
            r.plan.rps, r.plan.secs
        ),
        demo: false,
    };
    super::history::record_mark(&mark);
    lock(&HUB.series).annotate(mark);
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
    Checkout,
}

impl Scenario {
    pub const ALL: [Scenario; 7] = [
        Scenario::NPlusOne,
        Scenario::SlowQuery,
        Scenario::SlowEndpoint,
        Scenario::Checkout,
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
            Scenario::Checkout => "checkout",
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
            Scenario::Checkout => "Checkout fan-out",
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
            Scenario::Checkout => {
                "Twelve checkouts, each calling four simulated APIs: one after another, concurrently, and with retries."
            }
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
            Scenario::Checkout => "/dev/dashboard/upstreams",
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
            Scenario::Checkout => Some(p(
                &[
                    "/dev/lab/checkout?mode=sequential",
                    "/dev/lab/checkout?mode=parallel",
                    "/dev/lab/checkout?mode=retry",
                ],
                3,
                4,
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

// ---------------------------------------------------------------------------------------------
// Checkout fan-out: a page that calls four simulated APIs (served by this app under
// `/dev/lab/upstream/{name}`) through the outbound wrapper, one after another, concurrently, or
// with retries; the Upstreams control panel sets how each API behaves.
// ---------------------------------------------------------------------------------------------

/// The simulated APIs, in the order the checkout calls them.
pub const UPSTREAMS: [&str; 4] = ["inventory", "pricing", "payments", "shipping"];

/// How one simulated API behaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpSim {
    /// Typical latency, and how much more it may take (uniform; a tenth of calls take 2× that).
    pub latency_ms: u64,
    pub jitter_ms: u64,
    /// Percent of calls answered 503.
    pub error_pct: u32,
    /// Percent of calls that hang for `HANG` (past any sane timeout).
    pub hang_pct: u32,
}

/// A hung call waits this long before answering.
pub const HANG: Duration = Duration::from_secs(5);
/// The retry variant's timeout for shipping.
pub const RETRY_TIMEOUT: Duration = Duration::from_millis(800);

pub const DEFAULT_SIMS: [UpSim; 4] = [
    UpSim {
        latency_ms: 40,
        jitter_ms: 20,
        error_pct: 0,
        hang_pct: 0,
    },
    UpSim {
        latency_ms: 60,
        jitter_ms: 30,
        error_pct: 0,
        hang_pct: 0,
    },
    UpSim {
        latency_ms: 120,
        jitter_ms: 60,
        error_pct: 0,
        hang_pct: 0,
    },
    UpSim {
        latency_ms: 80,
        jitter_ms: 40,
        error_pct: 0,
        hang_pct: 0,
    },
];

/// The control panel's presets: (key, label, what it does).
pub const PRESETS: [(&str, &str, &str); 4] = [
    (
        "slow-payments",
        "Make payments slow",
        "payments at about 600 ms, p95 near 1.2 s",
    ),
    (
        "flaky-shipping",
        "Make shipping fail 20%",
        "one shipping call in five answers 503",
    ),
    (
        "timeouts",
        "Add timeouts",
        "shipping hangs on one call in four (the retry variant times out at 800 ms and retries)",
    ),
    ("reset", "Back to healthy", "every API fast and reliable"),
];

pub fn sims() -> [UpSim; 4] {
    lock(&HUB.lab.sims).unwrap_or(DEFAULT_SIMS)
}

/// Applies a preset (or `reset`); `false` for an unknown one.
pub fn preset(key: &str) -> bool {
    let mut s = sims();
    match key {
        "slow-payments" => {
            s[2] = UpSim {
                latency_ms: 600,
                jitter_ms: 650,
                error_pct: 0,
                hang_pct: 0,
            }
        }
        "flaky-shipping" => {
            s[3] = UpSim {
                error_pct: 20,
                ..s[3]
            }
        }
        "timeouts" => {
            s[3] = UpSim {
                hang_pct: 25,
                ..s[3]
            }
        }
        "reset" => s = DEFAULT_SIMS,
        _ => return false,
    }
    *lock(&HUB.lab.sims) = Some(s);
    true
}

/// Sets one API's behavior from the panel's form (each value capped).
pub fn set_sim(name: &str, latency_ms: u64, jitter_ms: u64, error_pct: u32, hang_pct: u32) -> bool {
    let Some(i) = UPSTREAMS.iter().position(|u| *u == name) else {
        return false;
    };
    let mut s = sims();
    // INVARIANT: the simulated APIs stay within these caps whatever the form says
    // (test: the_checkout_fan_out_traces_four_upstreams).
    s[i] = UpSim {
        latency_ms: latency_ms.min(5_000),
        jitter_ms: jitter_ms.min(5_000),
        error_pct: error_pct.min(100),
        hang_pct: hang_pct.min(100),
    };
    *lock(&HUB.lab.sims) = Some(s);
    true
}

#[derive(Deserialize)]
struct UpPath {
    name: String,
    action: String,
}

#[derive(Deserialize)]
struct UpItemPath {
    name: String,
    action: String,
    id: String,
}

/// `GET|POST /dev/lab/upstream/{name}/{action}`: one simulated API answering after its latency,
/// with its error and hang rates (the control panel's).
async fn upstream(
    axum::extract::Path(p): axum::extract::Path<UpPath>,
    headers: axum::http::HeaderMap,
) -> Response {
    simulate(&p.name, &p.action, &headers).await
}

async fn upstream_item(
    axum::extract::Path(p): axum::extract::Path<UpItemPath>,
    headers: axum::http::HeaderMap,
) -> Response {
    let _ = &p.id;
    simulate(&p.name, &p.action, &headers).await
}

async fn simulate(name: &str, action: &str, headers: &axum::http::HeaderMap) -> Response {
    let Some(i) = UPSTREAMS.iter().position(|u| *u == name) else {
        return (StatusCode::NOT_FOUND, "no such simulated API\n").into_response();
    };
    if let Some(tp) = headers.get("traceparent").and_then(|v| v.to_str().ok()) {
        *lock(&HUB.lab.last_traceparent) = Some(crate::kernel::ui::truncate(tp, 60));
    }
    let sim = sims()[i];
    let roll = rand::random_range(0..100_u32);
    if roll < sim.hang_pct {
        tokio::time::sleep(HANG).await;
    }
    let mut ms = sim.latency_ms + rand::random_range(0..=sim.jitter_ms);
    if rand::random_range(0..10) == 0 {
        ms += sim.jitter_ms;
    }
    tokio::time::sleep(Duration::from_millis(ms)).await;
    if rand::random_range(0..100_u32) < sim.error_pct {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "error": "unavailable", "service": name })),
        )
            .into_response();
    }
    let body = match name {
        "inventory" => serde_json::json!({ "sku": "SKU-MUG", "in_stock": 12 }),
        "pricing" => serde_json::json!({ "total_cents": 2_800, "currency": "USD" }),
        "payments" => serde_json::json!({ "charge": "ch_lab_0001", "status": "succeeded" }),
        _ => serde_json::json!({ "carrier": "Lab Post", "cents": 499, "days": 3 }),
    };
    let _ = action;
    axum::Json(body).into_response()
}

/// One call's result, for the page.
pub struct CallView {
    pub service: &'static str,
    pub what: String,
    pub outcome: String,
    pub dur: String,
    pub ok: bool,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/lab_checkout.html")]
struct CheckoutPage {
    ctx: crate::views::Ctx,
    mode: &'static str,
    modes: Vec<(&'static str, &'static str, bool)>,
    calls: Vec<CallView>,
    total: String,
    trace: u64,
    serving: bool,
}

#[derive(Deserialize, Default)]
struct CheckoutQuery {
    mode: Option<String>,
}

fn clients(base: &str) -> Option<[crate::kernel::outbound::Outbound; 4]> {
    use crate::kernel::outbound::Outbound;
    let mk =
        |name: &'static str| Outbound::new(name, &format!("{base}/dev/lab/upstream/{name}")).ok();
    Some([
        mk("inventory")?.timeout(Duration::from_secs(2)),
        // WHY no timeout: the naive variant's planted mistake ("Missing timeout").
        mk("pricing")?,
        mk("payments")?.timeout(Duration::from_secs(3)),
        mk("shipping")?.timeout(Duration::from_secs(2)),
    ])
}

type CallResult = Result<crate::kernel::outbound::Reply, crate::kernel::outbound::OutboundError>;

fn view(service: &'static str, what: &str, r: &CallResult, started: Instant) -> CallView {
    let (outcome, ok) = match r {
        Ok(reply) => (reply.status.to_string(), reply.is_success()),
        Err(e) => (e.to_string(), false),
    };
    CallView {
        service,
        what: what.to_owned(),
        outcome,
        dur: super::web::micros(started.elapsed().as_micros() as u64),
        ok,
    }
}

/// `GET /dev/lab/checkout?mode=sequential|parallel|retry`: the checkout screen.
async fn checkout(
    ctx: crate::views::Ctx,
    Query(q): Query<CheckoutQuery>,
) -> Result<Response, AppError> {
    let mode: &'static str = match q.mode.as_deref() {
        Some("parallel") => "parallel",
        Some("retry") => "retry",
        _ => "sequential",
    };
    checkout_body(ctx.titled("Lab: checkout"), mode)
        .instrument(tracing::info_span!("handler", handler = "lab::checkout"))
        .await
}

async fn checkout_body(ctx: crate::views::Ctx, mode: &'static str) -> Result<Response, AppError> {
    let started = Instant::now();
    let base = HUB.serving.get().cloned();
    let mut calls = Vec::new();
    if let Some(c) = base.as_deref().and_then(clients) {
        let [inventory, pricing, payments, shipping] = &c;
        let parcel = serde_json::json!({ "grams": 450, "to": "SE" });
        match mode {
            "sequential" => {
                // The naive version: four calls one after another.
                let t = Instant::now();
                let r = inventory.get("/items/1042").send().await;
                calls.push(view("inventory", "GET /items/{id}", &r, t));
                let t = Instant::now();
                let r = pricing.get("/quote").without_timeout().send().await;
                calls.push(view("pricing", "GET /quote (no timeout)", &r, t));
                let t = Instant::now();
                let r = payments
                    .post("/charge")
                    .json(&serde_json::json!({ "cents": 2_800 }))
                    .send()
                    .await;
                calls.push(view("payments", "POST /charge", &r, t));
                let t = Instant::now();
                let r = shipping.post("/rates").json(&parcel).send().await;
                calls.push(view("shipping", "POST /rates", &r, t));
            }
            _ => {
                // Independent calls together (tokio::join!); the charge needs the quote.
                let t = Instant::now();
                let ship = if mode == "retry" {
                    shipping
                        .post("/rates")
                        .json(&parcel)
                        .timeout(RETRY_TIMEOUT)
                        .retries(2)
                        .idempotent()
                } else {
                    shipping.post("/rates").json(&parcel)
                };
                let (inv, price, rates) = tokio::join!(
                    inventory.get("/items/1042").send(),
                    pricing.get("/quote").timeout(Duration::from_secs(2)).send(),
                    ship.send(),
                );
                calls.push(view("inventory", "GET /items/{id}", &inv, t));
                calls.push(view("pricing", "GET /quote", &price, t));
                calls.push(view(
                    "shipping",
                    if mode == "retry" {
                        "POST /rates (800 ms timeout, 2 retries)"
                    } else {
                        "POST /rates"
                    },
                    &rates,
                    t,
                ));
                let t = Instant::now();
                let r = payments
                    .post("/charge")
                    .json(&serde_json::json!({ "cents": 2_800 }))
                    .send()
                    .await;
                calls.push(view("payments", "POST /charge", &r, t));
            }
        }
    }
    let trace = super::trace::current_ctx().map_or(0, |c| c.id);
    let page = {
        let _render =
            tracing::info_span!("render", template = "kernel/dashboard/lab_checkout.html")
                .entered();
        CheckoutPage {
            ctx,
            mode,
            modes: vec![
                ("sequential", "One after another", mode == "sequential"),
                (
                    "parallel",
                    "Concurrently (tokio::join!)",
                    mode == "parallel",
                ),
                ("retry", "Concurrently, with retries", mode == "retry"),
            ],
            calls,
            total: super::web::micros(started.elapsed().as_micros() as u64),
            trace,
            serving: base.is_some(),
        }
        .render()?
    };
    Ok(axum::response::Html(page).into_response())
}

// ---------------------------------------------------------------------------------------------
// The lab's sign-in form: a stand-in for an auth sample, so the brute-force and credential
// stuffing detectors can be demoed without one. It reports through the same hook auth-core's
// optional wire uses.
// ---------------------------------------------------------------------------------------------

/// The lab form's own limits (development's rate limiter is lifted to 1,000).
pub const LAB_ACCOUNT_LIMIT: u32 = 10;
pub const LAB_IP_LIMIT: u32 = 20;
const LAB_PASSWORD: &str = "lab-correct-horse";

#[derive(Deserialize, Default)]
#[serde(default)]
struct SignInForm {
    email: String,
    password: String,
}

async fn sign_in_form() -> Response {
    axum::response::Html(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Lab sign-in</title></head><body><main><h1>Lab sign-in</h1>\
         <p>A stand-in for an auth sample: the attack simulator signs in here. Nothing is stored.</p>\
         <form method=\"post\" action=\"/dev/lab/sign-in\"><label>Email <input name=\"email\" type=\"email\" autocomplete=\"off\"></label> \
         <label>Password <input name=\"password\" type=\"password\" autocomplete=\"off\"></label> <button>Sign in</button></form></main></body></html>",
    )
    .into_response()
}

/// `POST /dev/lab/sign-in`: 422 for a wrong password, 429 past the lab's limits.
async fn sign_in(
    client: crate::controllers::application::ClientIp,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<SignInForm>,
) -> Response {
    let ip = super::threats::lab_client(&headers).or_else(|| client.0.map(|i| i.to_string()));
    let now = Utc::now().timestamp();
    let account = f.email.trim().to_ascii_lowercase();
    let over = |key: String, limit: u32| -> bool {
        let mut map = lock(&HUB.lab.sign_in);
        if map.len() >= 1_000 {
            map.clear();
        }
        let e = map.entry(key).or_insert((0, now));
        if now - e.1 > 60 {
            *e = (0, now);
        }
        e.0 >= limit
    };
    if over(format!("ip:{}", ip.as_deref().unwrap_or("")), LAB_IP_LIMIT) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "lab: too many attempts from this address\n",
        )
            .into_response();
    }
    if over(format!("acct:{account}"), LAB_ACCOUNT_LIMIT) {
        super::threats::auth_event_for("Throttled", &account, ip.as_deref());
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "lab: this account is paused\n",
        )
            .into_response();
    }
    if f.password == LAB_PASSWORD {
        super::threats::auth_event_for("SignedIn", &account, ip.as_deref());
        return (StatusCode::OK, "lab: signed in (nothing is stored)\n").into_response();
    }
    {
        let mut map = lock(&HUB.lab.sign_in);
        for k in [
            format!("ip:{}", ip.as_deref().unwrap_or("")),
            format!("acct:{account}"),
        ] {
            map.entry(k).or_insert((0, now)).0 += 1;
        }
    }
    super::threats::auth_event_for("SignInFailed", &account, ip.as_deref());
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        "lab: try another email address or password\n",
    )
        .into_response()
}

// ---------------------------------------------------------------------------------------------
// UX trouble: an app page with a slow button, a dead button, one that throws, and a link to a
// widget whose script only runs on a full page load (the boosted-navigation bug).
// ---------------------------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "kernel/dashboard/lab_ux.html")]
struct UxPage {
    ctx: crate::views::Ctx,
    script: String,
    simulate: bool,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/lab_ux_widget.html")]
struct UxWidgetPage {
    ctx: crate::views::Ctx,
    script: String,
    widget: String,
    simulate: bool,
}

#[derive(Deserialize, Default)]
struct UxQuery {
    simulate: Option<String>,
}

/// `GET /dev/lab/ux` (`?simulate=1`: the page plays the trouble by itself).
async fn ux(
    State(state): State<AppState>,
    ctx: crate::views::Ctx,
    Query(q): Query<UxQuery>,
) -> Result<Response, AppError> {
    Ok(axum::response::Html(
        UxPage {
            ctx: ctx.titled("Lab: UX trouble"),
            script: state.assets.path("kernel/lab-ux.js"),
            simulate: q.simulate.as_deref() == Some("1"),
        }
        .render()?,
    )
    .into_response())
}

/// `GET /dev/lab/ux/widget`: its "Show" button works on a full load, and does nothing after a
/// boosted link (its script binds on DOMContentLoaded, which a boosted swap never fires).
async fn ux_widget(
    State(state): State<AppState>,
    ctx: crate::views::Ctx,
    Query(q): Query<UxQuery>,
) -> Result<Response, AppError> {
    Ok(axum::response::Html(
        UxWidgetPage {
            ctx: ctx.titled("Lab: a widget"),
            script: state.assets.path("kernel/lab-ux.js"),
            widget: state.assets.path("kernel/lab-widget.js"),
            simulate: q.simulate.as_deref() == Some("1"),
        }
        .render()?,
    )
    .into_response())
}

/// `GET /dev/lab/ux/slow`: the slow button's request (1.4 s).
async fn ux_slow() -> Response {
    async {
        tokio::time::sleep(Duration::from_millis(1_400)).await;
    }
    .instrument(tracing::info_span!(
        "upstream_call",
        service = "reports (simulated)"
    ))
    .await;
    axum::response::Html("<p id=\"slow-result\">Report ready (it took 1.4 s).</p>").into_response()
}

// ---------------------------------------------------------------------------------------------
// The attack simulator: hostile traffic against this app's own address, so every security
// detector can be demoed and tested. Each request carries this process's secret, which lets it
// claim a TEST-NET client address (192.0.2.x, 198.51.100.x, 203.0.113.x: never a real one).
// ---------------------------------------------------------------------------------------------

/// The simulator's runs: (key, label, what it sends).
pub const ATTACKS: [(&str, &str, &str); 7] = [
    (
        "scanner",
        "Scanner run",
        "Nuclei-style probes for /.env, /wp-login.php, /.git/config…, TRACE and PROPFIND, and a 404 burst",
    ),
    (
        "brute",
        "Brute force",
        "25 wrong passwords for one account on the lab sign-in form",
    ),
    (
        "stuffing",
        "Credential stuffing",
        "30 accounts tried from one address, rotating user agents",
    ),
    (
        "probes",
        "Traversal and SQLi probes",
        "../ and encoded variants, UNION SELECT, <script> in query strings",
    ),
    (
        "csrf",
        "CSRF-less POSTs",
        "cross-site POSTs the CSRF layer refuses",
    ),
    (
        "session",
        "Stolen session",
        "one session cookie from three addresses and user agents",
    ),
    (
        "flood",
        "Request flood",
        "200 requests in a few seconds from one address",
    ),
];
/// Requests per run, at most.
pub const MAX_ATTACK: usize = 400;

/// One simulated request.
pub struct SimReq {
    pub method: &'static str,
    pub path: String,
    pub ip: &'static str,
    pub ua: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub form: Option<String>,
}

fn sim(method: &'static str, path: &str, ip: &'static str, ua: &'static str) -> SimReq {
    SimReq {
        method,
        path: path.to_owned(),
        ip,
        ua,
        headers: Vec::new(),
        form: None,
    }
}

/// What a run sends (paths only: the host is always this app's own address).
pub fn attack_plan(kind: &str) -> Option<Vec<SimReq>> {
    const NUCLEI: &str = "Mozilla/5.0 (compatible; Nuclei - Open-source project (github.com/projectdiscovery/nuclei))";
    const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
    let mut v = Vec::new();
    match kind {
        "scanner" => {
            for p in [
                "/.env",
                "/.git/config",
                "/wp-login.php",
                "/wp-admin/",
                "/xmlrpc.php",
                "/phpmyadmin/",
                "/server-status",
                "/actuator/health",
                "/cgi-bin/luci",
                "/.aws/credentials",
                "/config.php",
                "/vendor/phpunit/phpunit/src/Util/PHP/eval-stdin.php",
                "/.DS_Store",
                "/backup.zip",
                "/.htpasswd",
            ] {
                v.push(sim("GET", p, "203.0.113.7", NUCLEI));
            }
            for n in 0..20 {
                v.push(sim(
                    "GET",
                    &format!("/old-site/page-{n}.php"),
                    "203.0.113.7",
                    NUCLEI,
                ));
            }
            v.push(sim("TRACE", "/", "203.0.113.7", NUCLEI));
            v.push(sim("PROPFIND", "/", "203.0.113.7", NUCLEI));
        }
        "brute" => {
            for n in 0..25 {
                let mut r = sim(
                    "POST",
                    "/dev/lab/sign-in",
                    "198.51.100.23",
                    "python-requests/2.32.3",
                );
                r.form = Some(format!("email=victim%40example.com&password=guess{n}"));
                v.push(r);
            }
        }
        "stuffing" => {
            const UAS: [&str; 6] = [
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/129.0 Safari/537.36",
                "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0",
                "curl/8.7.1",
                "python-requests/2.32.3",
                "Go-http-client/1.1",
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0) Version/18.0 Mobile Safari/604.1",
            ];
            for n in 0..30 {
                let mut r = sim(
                    "POST",
                    "/dev/lab/sign-in",
                    "203.0.113.50",
                    UAS[n % UAS.len()],
                );
                r.form = Some(format!(
                    "email=user{n}%40example.com&password=Summer2026%21"
                ));
                v.push(r);
            }
        }
        "probes" => {
            for p in [
                "/static/..%2f..%2f..%2fetc%2fpasswd",
                "/dev/lab/orders?file=../../../../etc/passwd",
                "/dev/lab/orders?path=%252e%252e%252f%252e%252e%252fwin.ini",
                "/dev/lab/orders?id=1%27%20OR%20%271%27%3D%271",
                "/dev/lab/orders?q=1%20UNION%20SELECT%20password%20FROM%20users--",
                "/dev/lab/orders?sort=sleep(5)",
                "/?q=%3Cscript%3Ealert(document.cookie)%3C%2Fscript%3E",
                "/?next=javascript:alert(1)",
                "/dev/lab/orders?q=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E",
            ] {
                v.push(sim(
                    "GET",
                    p,
                    "192.0.2.66",
                    "sqlmap/1.8.9#stable (https://sqlmap.org)",
                ));
            }
        }
        "csrf" => {
            for _ in 0..10 {
                let mut r = sim("POST", "/theme", "198.51.100.99", BROWSER);
                r.headers
                    .push(("origin", "https://evil.example".to_owned()));
                r.headers.push(("sec-fetch-site", "cross-site".to_owned()));
                r.form = Some("theme=dark&return_to=%2F".to_owned());
                v.push(r);
            }
        }
        "session" => {
            let cookie = format!("lab_session=stolen-{:016x}", rand::random::<u64>());
            for (ip, ua) in [
                ("192.0.2.10", BROWSER),
                (
                    "192.0.2.11",
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/129.0 Safari/537.36",
                ),
                ("192.0.2.12", "curl/8.7.1"),
            ] {
                for _ in 0..3 {
                    let mut r = sim("GET", "/", ip, ua);
                    r.headers.push(("cookie", cookie.clone()));
                    v.push(r);
                }
            }
        }
        "flood" => {
            for _ in 0..200 {
                v.push(sim("GET", "/", "192.0.2.200", "Go-http-client/1.1"));
            }
        }
        "all" => {
            for (k, _, _) in ATTACKS {
                v.extend(attack_plan(k)?);
            }
        }
        _ => return None,
    }
    v.truncate(MAX_ATTACK);
    Some(v)
}

/// Starts a run in the background (one at a time).
pub fn start_attack(state: &AppState, kind: &str) -> Result<usize, String> {
    let plan = attack_plan(kind).ok_or_else(|| format!("No attack named {kind:?}."))?;
    if HUB.lab.attacking.swap(true, Ordering::SeqCst) {
        return Err("An attack simulation is running; wait for it to finish.".into());
    }
    let target = match HUB.serving.get() {
        Some(addr) => Target::Address(addr.clone()),
        None => match crate::routes::router(state.clone()) {
            Ok(r) => Target::InProcess(r),
            Err(e) => {
                HUB.lab.attacking.store(false, Ordering::SeqCst);
                return Err(format!("no router: {e}"));
            }
        },
    };
    let n = plan.len();
    let mark = super::series::Annotation {
        t: Utc::now().timestamp(),
        kind: "lab",
        label: format!("attack simulator: {kind}, {n} requests"),
        demo: false,
    };
    super::history::record_mark(&mark);
    lock(&HUB.series).annotate(mark);
    tokio::spawn(async move {
        run_attack(target, plan).await;
        HUB.lab.attacking.store(false, Ordering::SeqCst);
    });
    Ok(n)
}

/// Sends a plan's requests, 50 a second.
pub async fn run_attack(target: Target, plan: Vec<SimReq>) {
    let http = match &target {
        Target::Address(base) => client(base).ok(),
        Target::InProcess(_) => None,
    };
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    for r in plan {
        tick.tick().await;
        send_sim(&target, http.as_ref(), &r).await;
    }
}

/// One simulated request; its status (0: it failed to send).
pub async fn send_sim(target: &Target, http: Option<&reqwest::Client>, r: &SimReq) -> u16 {
    let secret = super::threats::SIM_SECRET.as_str();
    match (target, http) {
        (Target::Address(base), Some(http)) => {
            let Ok(method) = reqwest::Method::from_bytes(r.method.as_bytes()) else {
                return 0;
            };
            let mut req = http
                .request(method, format!("{base}{}", r.path))
                .header("user-agent", r.ua)
                .header(super::threats::SIM_HEADER, secret)
                .header(super::threats::SIM_CLIENT, r.ip);
            for (k, v) in &r.headers {
                req = req.header(*k, v);
            }
            if let Some(f) = &r.form {
                req = req
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(f.clone());
            }
            match req.send().await {
                Ok(res) => {
                    let s = res.status().as_u16();
                    let _ = res.bytes().await;
                    s
                }
                Err(_) => 0,
            }
        }
        (Target::InProcess(router), _) => {
            let mut b = Request::builder()
                .method(r.method)
                .uri(&r.path)
                .header("user-agent", r.ua)
                .header(super::threats::SIM_HEADER, secret)
                .header(super::threats::SIM_CLIENT, r.ip);
            for (k, v) in &r.headers {
                b = b.header(*k, v);
            }
            let body = match &r.form {
                Some(f) => {
                    b = b.header("content-type", "application/x-www-form-urlencoded");
                    Body::from(f.clone())
                }
                None => Body::empty(),
            };
            let Ok(req) = b.body(body) else {
                return 0;
            };
            match router.clone().oneshot(req).await {
                Ok(res) => {
                    let s = res.status().as_u16();
                    let _ = axum::body::to_bytes(res.into_body(), 1 << 20).await;
                    s
                }
                Err(_) => 0,
            }
        }
        _ => 0,
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
