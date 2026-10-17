//! `TestApp`: the app, a fresh migrated database and a cookie jar.
//! SQLite in-memory by default; `TEST_DATABASE_URL=postgres://…` runs on Postgres with one
//! locked, reset slot database per test (`test_database`).

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use migration::{Migrator, MigratorTrait};
use offrails_app::{
    config::Config,
    kernel::{self, cookie},
    routes,
    state::AppState,
};
use sea_orm::DatabaseConnection;
use tower::ServiceExt;

pub struct TestApp {
    pub state: AppState,
    router: Router,
    cookies: Mutex<BTreeMap<String, String>>,
    db_guard: DbGuard,
    /// Form posts send `Sec-Fetch-Site: same-origin` unless turned off.
    fetch_metadata: AtomicBool,
    /// `Origin` for form posts, if set.
    origin: Mutex<Option<String>>,
    /// SQL statements executed so far (the N+1 guard).
    queries: Arc<AtomicUsize>,
}

/// Holds a test's Postgres database (a no-op on SQLite) until it goes out of scope.
///
/// Postgres: a helper thread with its own runtime holds the database's advisory lock, so
/// nothing depends on the test's runtime, which is gone by the time `Drop` runs. `Drop` tells
/// it to unlock, and joins it. A killed run's locks go with its connections.
pub struct DbGuard(Option<PgSlot>);

struct PgSlot {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DbGuard {
    pub fn is_sqlite(&self) -> bool {
        self.0.is_none()
    }
}

impl Drop for DbGuard {
    fn drop(&mut self) {
        let Some(mut slot) = self.0.take() else {
            return;
        };
        drop(slot.release.take());
        if let Some(thread) = slot.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A migrated, empty database, and the guard that holds it (keep it alive for the test).
///
/// SQLite: a fresh in-memory database, migrated. Postgres (`TEST_DATABASE_URL`): no
/// `CREATE DATABASE` and no migrations per test. The migrations run once into a template
/// database (`test_<crate>_<tag>_tpl`, `<tag>` a hash of the migration files, so a changed
/// migration builds a new one); each concurrent test locks a slot database
/// (`test_<crate>_<tag>_<n>`), cloned from the template the first time and reused after that;
/// and `offrails_test.reset()` puts the slot back to the just-migrated state (tables emptied,
/// the migrations' seed rows and the sequences restored) before the test gets it. A slot whose
/// migrations a test rolled back is dropped and cloned again. Isolated as before, minus the
/// DDL: testing.md#postgres-test-databases.
pub async fn test_database() -> (DatabaseConnection, DbGuard) {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(admin_url) if !admin_url.is_empty() => {
            let (url, slot) = checkout_postgres_slot(admin_url);
            let db = kernel::db::connect(&url, 5)
                .await
                .expect("connect to the test database");
            (db, DbGuard(Some(slot)))
        }
        _ => {
            let db = kernel::db::connect("sqlite::memory:", 1)
                .await
                .expect("sqlite");
            Migrator::up(&db, None).await.expect("migrate");
            (db, DbGuard(None))
        }
    }
}

/// Locks a slot on a helper thread and returns its URL once it is reset.
fn checkout_postgres_slot(admin_url: String) -> (String, PgSlot) {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = ready_tx.send(Err("a runtime for the test database".into()));
            return;
        };
        match rt.block_on(lock_slot(&admin_url)) {
            Ok((admin, key, url)) => {
                let _ = ready_tx.send(Ok(url));
                let _ = release_rx.recv(); // Err when the guard drops the sender
                rt.block_on(async {
                    let _ = kernel::db::advisory_unlock(&admin, key).await;
                    let _ = admin.close().await;
                });
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        }
    });
    let url = ready_rx
        .recv()
        .expect("test database thread")
        .unwrap_or_else(|e| panic!("Postgres test database (TEST_DATABASE_URL): {e}"));
    (
        url,
        PgSlot {
            release: Some(release_tx),
            thread: Some(thread),
        },
    )
}

/// `test_<crate>_`: apps sharing a Postgres server never touch each other's databases.
fn test_db_prefix() -> String {
    format!("test_{}_", env!("CARGO_PKG_NAME").replace('-', "_"))
}

/// FNV-1a: stable across Rust versions and platforms (std's hasher is neither).
fn fnv1a(bytes: impl IntoIterator<Item = u8>, mut hash: u64) -> u64 {
    for b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// What the template is built from: the migration names, every file under `migration/src`
/// (editing an uncommitted migration changes the tag too), and the reset's own version.
fn migration_tag() -> String {
    let mut hash = fnv1a(*b"offrails-test-reset-v3", 0xcbf2_9ce4_8422_2325);
    for m in Migrator::migrations() {
        hash = fnv1a(m.name().bytes().chain([0]), hash);
    }
    let mut files = Vec::new();
    let mut dirs = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migration/src"
    ))];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    for f in files {
        hash = fnv1a(f.to_string_lossy().bytes().chain([0]), hash);
        hash = fnv1a(std::fs::read(&f).unwrap_or_default(), hash);
    }
    format!("{:012x}", hash >> 16)
}

/// `postgres://…/<admin db>` with another database name.
fn database_url(admin_url: &str, name: &str) -> String {
    let (base, _) = admin_url
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL names a database");
    format!("{base}/{name}")
}

/// Locks a free slot (connection `admin` holds the lock at `key`) and returns its URL, cloned
/// from the template if it doesn't exist yet, reset if it does.
async fn lock_slot(admin_url: &str) -> Result<(DatabaseConnection, i64, String), sea_orm::DbErr> {
    let admin = kernel::db::connect(admin_url, 1).await?;
    let prefix = test_db_prefix();
    let tag = migration_tag();
    let template = format!("{prefix}{tag}_tpl");
    // Advisory locks are keyed by the admin database: one key range per app.
    let base_key = (fnv1a(prefix.bytes(), 0xcbf2_9ce4_8422_2325) & 0x3fff_ffff_ffff_0000) as i64;
    ensure_template(&admin, admin_url, &prefix, &tag, &template, base_key).await?;

    let mut slot = 0;
    while !kernel::db::try_advisory_lock(&admin, base_key + 1 + slot).await? {
        slot += 1;
    }
    let key = base_key + 1 + slot;
    let name = format!("{prefix}{tag}_{slot}");
    let url = database_url(admin_url, &name);
    if kernel::db::database_exists(&admin, &name).await? {
        let db = kernel::db::connect(&url, 1).await?;
        // Refuses when a test changed the schema (rolled a migration back, made a table).
        let current = kernel::db::reset_test_database(&db).await.is_ok();
        let _ = db.close().await;
        if current {
            return Ok((admin, key, url));
        }
        // The schema drifted (or the slot is broken): clone it again.
        kernel::db::drop_database(&admin, &name).await?;
    }
    let mut attempt = 0;
    loop {
        match kernel::db::create_database_from(&admin, &name, &template).await {
            Ok(()) => {
                kernel::db::skip_commit_flush(&admin, &name).await?;
                return Ok((admin, key, url));
            }
            // The template is busy (another run is building or cleaning) or was dropped.
            Err(_) if attempt < 50 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
                ensure_template(&admin, admin_url, &prefix, &tag, &template, base_key).await?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Builds the template once per migration set, under an advisory lock (concurrent tests wait
/// for the first one): migrate a build database, install the reset, then rename it, so a
/// half-built template never has the final name. Then drops the idle databases of older sets.
async fn ensure_template(
    admin: &DatabaseConnection,
    admin_url: &str,
    prefix: &str,
    tag: &str,
    template: &str,
    lock_key: i64,
) -> Result<(), sea_orm::DbErr> {
    if kernel::db::database_exists(admin, template).await? {
        return Ok(());
    }
    kernel::db::advisory_lock(admin, lock_key).await?;
    let built = async {
        if kernel::db::database_exists(admin, template).await? {
            return Ok(());
        }
        let build = format!("{prefix}{tag}_b{}", std::process::id());
        kernel::db::drop_database(admin, &build).await?;
        kernel::db::create_database(admin, &build).await?;
        let migrated = async {
            let db = kernel::db::connect(&database_url(admin_url, &build), 1).await?;
            Migrator::up(&db, None).await?;
            kernel::db::install_test_reset(&db).await?;
            db.close().await
        }
        .await;
        if let Err(e) = migrated {
            let _ = kernel::db::drop_database(admin, &build).await;
            return Err(e);
        }
        kernel::db::rename_database(admin, &build, template).await?;
        kernel::db::drop_idle_stale_test_databases(admin, prefix, tag).await?;
        Ok(())
    }
    .await;
    let _ = kernel::db::advisory_unlock(admin, lock_key).await;
    built
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_config(Config::test()).await
    }

    /// For tests that need another environment, e.g. development (dev tools, local mailbox)
    /// or production (they must not exist).
    pub async fn with_config(config: Config) -> Self {
        let (mut db, db_guard) = test_database().await;
        // The callback needs `&mut`, so it's set before the connection is cloned into the state.
        let queries = Arc::new(AtomicUsize::new(0));
        let counter = queries.clone();
        db.set_metric_callback(move |_info| {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let state = AppState::new(config, db).expect("app state");
        let router = routes::router(state.clone()).expect("router");
        Self {
            state,
            router,
            cookies: Mutex::new(BTreeMap::new()),
            db_guard,
            fetch_metadata: AtomicBool::new(true),
            origin: Mutex::new(None),
            queries,
        }
    }

    /// Later form posts omit `Sec-Fetch-Site`, so CSRF falls back to the `Origin` header.
    pub fn without_fetch_metadata(&self) -> &Self {
        self.fetch_metadata.store(false, Ordering::Relaxed);
        self
    }

    /// Later form posts send this `Origin`.
    pub fn with_origin(&self, origin: &str) -> &Self {
        *self.origin.lock().expect("origin") = Some(origin.to_owned());
        self
    }

    /// SQL statements executed so far.
    pub fn query_count(&self) -> usize {
        self.queries.load(Ordering::Relaxed)
    }

    /// The N+1 guard: seeds 1 record, counts the queries of `GET path`, seeds 19 more, counts
    /// again, and asserts the two counts are equal.
    pub async fn assert_query_count_stable(&self, path: &str, seed: impl AsyncFn(usize)) {
        seed(1).await;
        let before = self.query_count();
        self.get(path).await.assert_ok();
        let with_one = self.query_count() - before;

        seed(19).await;
        let before = self.query_count();
        self.get(path).await.assert_ok();
        let with_twenty = self.query_count() - before;
        assert_eq!(
            with_one, with_twenty,
            "GET {path} runs {with_one} queries with 1 record and {with_twenty} with 20: an N+1"
        );
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.state.db
    }

    pub fn on_sqlite(&self) -> bool {
        self.db_guard.is_sqlite()
    }

    // ----- requests -----

    pub async fn get(&self, path: &str) -> TestResponse {
        self.send(Method::GET, path, &[], None).await
    }

    /// A boosted/htmx request (`HX-Request: true`).
    pub async fn get_htmx(&self, path: &str) -> TestResponse {
        self.send(Method::GET, path, &[("hx-request", "true")], None)
            .await
    }

    pub async fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        self.send(Method::GET, path, headers, None).await
    }

    pub async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> TestResponse {
        self.post_form_with_headers(path, form, &[]).await
    }

    pub async fn post_form_with_headers(
        &self,
        path: &str,
        form: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", cookie::encode(k), cookie::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(Method::POST, path, headers, Some(body)).await
    }

    /// Any method with an empty form body: the route sweeps request every `ROUTES` row with its
    /// own method (an API's `PATCH` and `DELETE` too).
    pub async fn request_method(&self, method: &str, path: &str) -> TestResponse {
        let method = Method::from_bytes(method.as_bytes()).expect("an HTTP method");
        self.send(method, path, &[], Some(String::new())).await
    }

    /// A JSON request (`body` serialized, `content-type: application/json`) with extra headers,
    /// e.g. `("authorization", "Bearer …")`. No cookies and no fetch metadata: an API client.
    pub async fn send_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let method = Method::from_bytes(method.as_bytes()).expect("an HTTP method");
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let request = match body {
            Some(json) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request");
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes();
        TestResponse {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    /// Status and headers without reading the body (for endless responses such as SSE).
    pub async fn request_head(
        &self,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap) {
        let mut builder = Request::builder().method(Method::GET).uri(path);
        let jar = self.cookies.lock().expect("cookie jar").clone();
        if !jar.is_empty() {
            let value = jar
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, value);
        }
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("infallible");
        let (parts, _body) = response.into_parts();
        (parts.status, parts.headers)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method.clone()).uri(path);
        // Form posts carry what a real browser sends, so CSRF passes unless a test turns it off
        // (`without_fetch_metadata`) or overrides a header.
        let sent = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if method != Method::GET
            && !sent("sec-fetch-site")
            && self.fetch_metadata.load(Ordering::Relaxed)
        {
            builder = builder.header("sec-fetch-site", "same-origin");
        }
        if method != Method::GET
            && !sent("origin")
            && let Some(origin) = self.origin.lock().expect("origin").clone()
        {
            builder = builder.header(header::ORIGIN, origin);
        }
        let jar = self.cookies.lock().expect("cookie jar").clone();
        if !jar.is_empty() {
            let value = jar
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, value);
        }
        for (k, v) in headers {
            // An empty value means "omit this header" (used to exercise the Origin fallback).
            if v.is_empty() {
                continue;
            }
            builder = builder.header(*k, *v);
        }
        let request = match body {
            Some(b) => builder
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(b)),
            None => builder.body(Body::empty()),
        }
        .expect("request");

        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes();
        self.store_cookies(&parts.headers);
        TestResponse {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    fn store_cookies(&self, headers: &HeaderMap) {
        let mut jar = self.cookies.lock().expect("cookie jar");
        for value in headers.get_all(header::SET_COOKIE) {
            let Ok(raw) = value.to_str() else { continue };
            let mut parts = raw.split(';').map(str::trim);
            let Some((name, val)) = parts.next().and_then(|p| p.split_once('=')) else {
                continue;
            };
            let expired = parts.any(|a| a.eq_ignore_ascii_case("max-age=0"));
            if expired || val.is_empty() {
                jar.remove(name);
            } else {
                jar.insert(name.to_owned(), val.to_owned());
            }
        }
    }

    // ----- cookies & sessions -----

    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies.lock().expect("cookie jar").get(name).cloned()
    }

    pub fn set_cookie(&self, name: &str, value: &str) {
        self.cookies
            .lock()
            .expect("cookie jar")
            .insert(name.to_owned(), value.to_owned());
    }

    pub fn clear_cookies(&self) {
        self.cookies.lock().expect("cookie jar").clear();
    }

    // samples: test support
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl TestResponse {
    pub fn assert_ok(&self) -> &Self {
        self.assert_status(StatusCode::OK)
    }

    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        assert_eq!(
            self.status, expected,
            "unexpected status; body:\n{}",
            self.body
        );
        self
    }

    /// 303 See Other to `path`.
    pub fn assert_redirect_to(&self, path: &str) -> &Self {
        assert_eq!(
            self.status,
            StatusCode::SEE_OTHER,
            "expected a redirect; body:\n{}",
            self.body
        );
        assert_eq!(self.location(), Some(path), "redirected somewhere else");
        self
    }

    pub fn assert_contains(&self, text: &str) -> &Self {
        assert!(
            self.body.contains(text),
            "body does not contain {text:?}:\n{}",
            self.body
        );
        self
    }

    /// Visible text: escapes `text` the way Askama 0.16 does (`&#60; &#62; &#38; &#34; &#39;`)
    /// before looking for it, so `assert_text("we've sent it")` matches `we&#39;ve sent it`.
    /// Use `assert_contains` for markup.
    pub fn assert_text(&self, text: &str) -> &Self {
        let escaped = escape_like_askama(text);
        assert!(
            self.body.contains(&escaped),
            "body does not contain the text {text:?} (as {escaped:?}):\n{}",
            self.body
        );
        self
    }

    pub fn assert_not_contains(&self, text: &str) -> &Self {
        assert!(
            !self.body.contains(text),
            "body unexpectedly contains {text:?}:\n{}",
            self.body
        );
        self
    }

    pub fn location(&self) -> Option<&str> {
        self.headers
            .get(header::LOCATION)
            .and_then(|v: &HeaderValue| v.to_str().ok())
    }

    /// The body as JSON (agents' endpoints, API errors).
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("not the expected JSON ({e}): {}", self.body))
    }

    pub fn set_cookies(&self) -> Vec<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect()
    }
}

fn escape_like_askama(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&#60;"),
            '>' => out.push_str("&#62;"),
            '&' => out.push_str("&#38;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Awaits a condition: polled every 20ms, 10s deadline. The only way runtime tests wait;
/// never `sleep`.
pub async fn wait_until(what: &str, check: impl AsyncFn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if check().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
