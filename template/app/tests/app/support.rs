//! `TestApp`: the app, a fresh migrated database and a cookie jar.
//! SQLite in-memory by default; `TEST_DATABASE_URL=postgres://…` runs on Postgres with one
//! `test_<uuid>` database per test.

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

/// Drops a test's Postgres database when it goes out of scope (a no-op on SQLite).
///
/// A task spawned on the test's runtime is cancelled when that runtime shuts down, and the
/// database would leak. So: a dedicated thread with its own runtime, joined before `Drop`
/// returns. Leftovers (a killed run) are removed by `app task test_db_cleanup`.
pub struct DbGuard(Option<(String, String)>);

impl DbGuard {
    pub fn is_sqlite(&self) -> bool {
        self.0.is_none()
    }
}

impl Drop for DbGuard {
    fn drop(&mut self) {
        let Some((admin_url, name)) = self.0.take() else {
            return;
        };
        let handle = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                if let Ok(admin) = kernel::db::connect(&admin_url, 1).await {
                    let _ = kernel::db::drop_database(&admin, &name).await;
                }
            });
        });
        let _ = handle.join();
    }
}

/// A migrated, empty database, and the guard that drops it (keep it alive for the test).
pub async fn test_database() -> (DatabaseConnection, DbGuard) {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(admin_url) if !admin_url.is_empty() => {
            let admin = kernel::db::connect(&admin_url, 2)
                .await
                .expect("connect to postgres");
            // `test_<crate>_<hex>`: apps sharing a Postgres server never touch each other's databases.
            let name = format!(
                "test_{}_{}",
                env!("CARGO_PKG_NAME").replace('-', "_"),
                uuid::Uuid::new_v4().simple()
            );
            kernel::db::create_database(&admin, &name)
                .await
                .expect("create test database");
            let (base, _) = admin_url
                .rsplit_once('/')
                .expect("postgres url has a database");
            let db = kernel::db::connect(&format!("{base}/{name}"), 5)
                .await
                .expect("connect to test database");
            Migrator::up(&db, None).await.expect("migrate");
            (db, DbGuard(Some((admin_url, name))))
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
