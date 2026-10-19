//! The dev dashboard's signals: outbound calls (the `kernel::outbound` wrapper, the Upstreams view,
//! the checkout fan-out), security (the detectors over synthetic streams, the attack simulator,
//! client-address privacy) and the frontend (the beacon's endpoint, its validation, insights). Each
//! test named in samples/dev-dashboard/CHECKS.md is here or in dev_dashboard_test.rs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use offrails_host::config::{AppEnv, Config, DashboardConfig};
use offrails_host::kernel::dashboard::threats::{self, Obs, Threats, sig};
use offrails_host::kernel::dashboard::trace::DashboardLayer;
use offrails_host::kernel::dashboard::{
    HUB, ai_tools, insights, lab, lock, rum, upstreams, waterfall,
};
use offrails_host::kernel::outbound::{self, Outbound, OutboundError};
use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;

use crate::support::TestApp;

async fn dev_app() -> TestApp {
    TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await
}

const ADMIN_PASSWORD: &str = "correct horse battery staple";

fn production(dashboard: DashboardConfig) -> Config {
    Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        trusted_proxy: true,
        dashboard,
        ..Config::test()
    }
}

fn gated() -> DashboardConfig {
    DashboardConfig {
        in_production: true,
        admin_password: Some(ADMIN_PASSWORD.into()),
        ..DashboardConfig::default()
    }
}

fn basic() -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("admin:{ADMIN_PASSWORD}"))
    )
}

/// The dashboard's layer behind development's log filter, as `main` installs it: the process's
/// global default, once (nextest runs each test in a process of its own).
// WHY global, not `set_default` (this thread only): the SQLite driver's worker thread, and other
// threads a request hands work to, hold clones of the request's spans and can drop the last one.
// The registry then closes the span's parent through *that* thread's default subscriber, which a
// thread-local one isn't: the parents (handler, the methods above the last query, an outbound
// call) never closed, and a loaded run's trace lost them.
fn traced() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(DashboardLayer).with(
                tracing_subscriber::EnvFilter::new("info,offrails_host=debug,tower_http=debug"),
            ),
        );
    });
}

/// Serves `router` on a loopback port; its base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    format!("http://{addr}")
}

// ---- outbound calls ---------------------------------------------------------------------------

/// The wrapper: route templates without ids or queries, a valid `traceparent`, timeouts, retries
/// of safe calls only, no redirects followed, no credentials in a base URL, a size cap; every call
/// reported to the Upstreams view.
#[tokio::test]
async fn outbound_calls_are_traced_redacted_and_bounded() {
    traced();
    let _app = dev_app().await;
    let flaky = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let (f1, f2, s1) = (flaky.clone(), flaky.clone(), seen.clone());
    let mock = Router::new()
        .route(
            "/items/{id}",
            get(move |headers: axum::http::HeaderMap| {
                let seen = s1.clone();
                async move {
                    *seen.lock().unwrap() = headers
                        .get("traceparent")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    axum::Json(json!({ "ok": true }))
                }
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                "late"
            }),
        )
        .route(
            "/flaky",
            get(move || {
                let n = f1.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }
            })
            .post(move || {
                f2.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::SERVICE_UNAVAILABLE }
            }),
        )
        .route(
            "/moved",
            get(|| async { axum::response::Redirect::temporary("https://elsewhere.example/") }),
        )
        .route(
            "/big",
            get(|| async { vec![b'x'; outbound::MAX_BODY + 10] }),
        );
    let base = serve(mock).await;
    // Credentials in a base URL are refused.
    assert!(matches!(
        Outbound::new("svc", "https://user:secret@api.example.com"),
        Err(OutboundError::Url { .. })
    ));
    assert!(Outbound::new("svc", "ftp://api.example.com").is_err());
    let api = Outbound::new("catalog", &base).unwrap();
    let reply = api
        .get("/items/42?api_key=s3cret-value")
        .bearer("tok-ABCDEF")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    let tp = seen.lock().unwrap().clone();
    assert!(outbound::trace_id_of(&tp).is_some(), "traceparent: {tp:?}");
    // A timeout.
    let late = api
        .get("/slow")
        .timeout(Duration::from_millis(50))
        .send()
        .await;
    assert!(
        matches!(late, Err(OutboundError::Timeout { .. })),
        "{late:?}"
    );
    // A GET retries on 503 and gets its answer; a POST (not idempotent) doesn't retry.
    let ok = api.get("/flaky").retries(2).send().await.unwrap();
    assert_eq!(ok.status, 200);
    let before = flaky.load(Ordering::SeqCst);
    let post = api.post("/flaky").retries(2).send().await.unwrap();
    assert_eq!(post.status, 503);
    assert_eq!(flaky.load(Ordering::SeqCst), before + 1, "a POST retried");
    // Redirects aren't followed; a body past the cap is refused; no timeout is allowed (and seen).
    assert_eq!(api.get("/moved").send().await.unwrap().status, 307);
    assert!(matches!(
        api.get("/big").send().await,
        Err(OutboundError::TooLarge { .. })
    ));
    api.get("/items/7").without_timeout().send().await.unwrap();
    let text = upstreams::list_text();
    for leak in ["s3cret", "api_key", "tok-ABCDEF", "/items/42", "/items/7"] {
        assert!(!text.contains(leak), "{leak} in:\n{text}");
    }
    assert!(text.contains("catalog GET /items/{id}"), "{text}");
    {
        let u = lock(&HUB.upstreams);
        let flaky_up = u
            .list()
            .into_iter()
            .find(|x| x.route == "/flaky" && x.method == "GET")
            .unwrap();
        assert_eq!(flaky_up.retries, 2);
        let slow = u.list().into_iter().find(|x| x.route == "/slow").unwrap();
        assert_eq!(slow.timeouts, 1);
        let items = u
            .list()
            .into_iter()
            .find(|x| x.route == "/items/{id}")
            .unwrap();
        assert_eq!(items.no_timeout, 1);
        assert_eq!(items.calls, 2);
    }
    let found = upstreams::findings(&[]);
    assert!(found.iter().any(|f| {
        f.title
            .starts_with("Missing timeout: catalog GET /items/{id}")
    }));
}

/// The lab's checkout calls four simulated APIs: one after another (the waterfall says how much
/// running them together would save), concurrently, and with retries and timeouts; the Upstreams
/// view, its dependency map, the insights and the AI tools name the slow API.
#[tokio::test]
async fn the_checkout_fan_out_traces_four_upstreams() {
    traced();
    let app = dev_app().await;
    let base = serve(offrails_host::routes::router(app.state.clone()).unwrap()).await;
    let _ = HUB.serving.set(base.clone());
    for name in lab::UPSTREAMS {
        assert!(lab::set_sim(name, 25, 0, 0, 0));
    }
    // INVARIANT: the panel's caps.
    assert!(lab::set_sim("payments", 99_999, 99_999, 500, 500));
    assert_eq!(lab::sims()[2].latency_ms, 5_000);
    assert!(lab::set_sim("payments", 25, 0, 0, 0));
    assert!(!lab::set_sim("nope", 1, 1, 1, 1));
    let http = reqwest::Client::new();
    let get = |q: &'static str| {
        let (http, base) = (http.clone(), base.clone());
        async move {
            let res = http
                .get(format!("{base}/dev/lab/checkout?mode={q}"))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 200);
            res.text().await.unwrap()
        }
    };
    let page = get("sequential").await;
    assert!(
        page.contains("payments") && page.contains("Its waterfall"),
        "{page}"
    );
    let checkout = |n: usize| {
        lock(&HUB.traces)
            .list(false)
            .into_iter()
            .filter(|t| t.route == "/dev/lab/checkout")
            .nth(n)
            .unwrap()
    };
    let seq = checkout(0);
    let root = waterfall::tree(&seq, true);
    let s =
        upstreams::sequential(&root).expect("the naive checkout runs its calls one after another");
    assert_eq!(s.calls.len(), 4, "{s:?}");
    assert!(s.saving_us >= 50_000, "{s:?}");
    let view = waterfall::view(
        &seq,
        true,
        "/dev/dashboard",
        false,
        true,
        false,
        String::new(),
    );
    assert!(
        view.rows
            .iter()
            .any(|r| r.kind == "http" && r.label.starts_with("POST payments /charge → 200")),
        "{:?}",
        view.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(view.rows.iter().any(|r| r.host.starts_with("127.0.0.1:")));
    assert!(view.seq.is_some());
    // The traceparent the simulated APIs saw carries the checkout's request id.
    let tp = lock(&HUB.lab.last_traceparent).clone().unwrap();
    assert_eq!(
        outbound::trace_id_of(&tp),
        upstreams::hex_id(&seq.request_id).as_deref()
    );
    // Concurrently: no "sequential" note for the three independent calls.
    get("parallel").await;
    let par = checkout(0);
    let root = waterfall::tree(&par, true);
    assert!(upstreams::concurrent(&root) >= 3);
    assert!(upstreams::sequential(&root).is_none_or(|s| s.calls.len() < 4));
    // Retries and timeouts: shipping hangs, the retry variant times out at 800 ms and tries again.
    assert!(lab::set_sim("shipping", 10, 0, 0, 100));
    get("retry").await;
    let retried = checkout(0);
    let view = waterfall::view(
        &retried,
        true,
        "/dev/dashboard",
        false,
        true,
        false,
        String::new(),
    );
    let attempts: Vec<_> = view
        .rows
        .iter()
        .filter(|r| r.label.starts_with("attempt "))
        .collect();
    assert_eq!(
        attempts.len(),
        3,
        "{:?}",
        view.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(attempts.iter().all(|r| r.timeout && r.failed));
    // Slow payments: the Upstreams view, the insight and the tools name it.
    assert!(lab::set_sim("shipping", 10, 0, 0, 0));
    assert!(lab::set_sim("payments", 400, 0, 0, 0));
    for _ in 0..3 {
        get("sequential").await;
    }
    let found = insights::findings(&app.state).await;
    let titles: Vec<&str> = found.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("Slow upstream: payments POST /charge")),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|t| t
                .starts_with("Sequential calls that could be parallel on GET /dev/lab/checkout")),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("Missing timeout: pricing GET /quote")),
        "{titles:?}"
    );
    let slow = found
        .iter()
        .find(|f| f.title.starts_with("Slow upstream: payments"))
        .unwrap();
    assert!(
        slow.evidence.contains("called by GET /dev/lab/checkout"),
        "{}",
        slow.evidence
    );
    app.get("/dev/dashboard/upstreams")
        .await
        .assert_ok()
        .assert_contains("class=\"up-map\"")
        .assert_contains("Calls that could run concurrently")
        .assert_contains("/dev/dashboard/traces?upstream=payments")
        .assert_text("GET /dev/lab/checkout");
    app.get("/dev/dashboard/traces?upstream=payments")
        .await
        .assert_ok()
        .assert_contains("/dev/lab/checkout");
    let out = ai_tools::call(
        &app.state,
        false,
        false,
        "get_upstream",
        &json!({ "service": "payments" }),
    )
    .await;
    assert!(
        !out.is_error && out.text.contains("[route:GET /dev/lab/checkout]"),
        "{}",
        out.text
    );
    let out = ai_tools::call(&app.state, true, false, "list_upstreams", &json!({})).await;
    assert!(out.text.starts_with("service method route"), "{}", out.text);
    assert!(
        out.text.contains("- payments POST /charge (127.0.0.1:"),
        "{}",
        out.text
    );
}

/// Twelve checkouts: the scenario stays within the generator's caps and asks only for this app's
/// own GET routes.
#[test]
fn the_checkout_scenario_targets_this_app_only() {
    let s = lab::Scenario::parse("checkout").unwrap();
    let p = s.plan().unwrap();
    assert!(p.paths.iter().all(|path| lab::allowed_path(path)));
    assert!(p.rps <= lab::MAX_RPS && p.secs <= lab::MAX_SECS);
}

// ---- security ---------------------------------------------------------------------------------

fn kinds(t: &Threats, now: i64) -> Vec<&'static str> {
    t.findings(now).iter().map(|f| f.kind).collect()
}

/// Every detector fires on a synthetic stream of its pattern, and none on ordinary browsing.
#[test]
fn security_detectors_fire_on_synthetic_streams() {
    let now = 1_800_000_000;
    // Ordinary browsing: 30 addresses, a few pages each, an occasional 404, one session each.
    let mut quiet = Threats::default();
    for i in 0..30 {
        let ip = format!("198.51.100.{i}");
        for (k, p) in ["/", "/orders", "/orders/7", "/account", "/favicon.png"]
            .iter()
            .enumerate()
        {
            let status = if *p == "/favicon.png" { 404 } else { 200 };
            quiet.ingest(Obs::synthetic(
                now - 60 + k as i64,
                &ip,
                "GET",
                p,
                status,
                0,
            ));
        }
    }
    assert!(
        quiet.findings(now).is_empty(),
        "false positives: {:?}",
        kinds(&quiet, now)
    );

    let mut t = Threats::default();
    for (i, p) in [
        "/.env",
        "/.git/config",
        "/wp-login.php",
        "/phpmyadmin/",
        "/actuator/env",
    ]
    .iter()
    .enumerate()
    {
        t.ingest(Obs::synthetic(
            now - 30 + i as i64,
            "203.0.113.7",
            "GET",
            p,
            404,
            0,
        ));
    }
    for (ip, path) in [
        ("192.0.2.66", "/files?name=..%2f..%2fetc%2fpasswd"),
        ("192.0.2.67", "/orders?id=1%27%20OR%20%271%27%3D%271"),
        ("192.0.2.68", "/search?q=%3Cscript%3Ealert(1)%3C/script%3E"),
    ] {
        t.ingest(Obs::synthetic(now - 20, ip, "GET", path, 404, 0));
    }
    t.ingest(Obs::synthetic(
        now - 10,
        "192.0.2.70",
        "TRACE",
        "/",
        405,
        sig::ODD_METHOD,
    ));
    for _ in 0..6 {
        t.ingest(Obs::synthetic(
            now - 9,
            "198.51.100.99",
            "POST",
            "/theme",
            403,
            sig::CSRF,
        ));
    }
    for _ in 0..12 {
        t.ingest(Obs::synthetic(
            now - 8,
            "198.51.100.50",
            "POST",
            "/api",
            429,
            sig::RATE_LIMITED,
        ));
    }
    for _ in 0..200 {
        t.ingest(Obs::synthetic(now - 5, "192.0.2.200", "GET", "/", 200, 0));
    }
    t.ingest(Obs::synthetic(now - 4, "192.0.2.201", "GET", "/", 200, 0));
    t.ingest(Obs::synthetic(now - 4, "192.0.2.202", "GET", "/", 200, 0));
    // Brute force: one account, many failures, from two addresses.
    for i in 0..12 {
        t.ingest_auth(threats::AuthObs {
            at: now - 40 + i,
            ip: threats::keyed("198.51.100.23".parse::<std::net::IpAddr>().unwrap()),
            ip_label: "198.51.100.23".into(),
            account: Some(threats::keyed(("account", 7_i64))),
            kind: "failed",
        });
    }
    // Credential stuffing: one address, many accounts.
    for i in 0..6_i64 {
        t.ingest_auth(threats::AuthObs {
            at: now - 30,
            ip: threats::keyed("203.0.113.50".parse::<std::net::IpAddr>().unwrap()),
            ip_label: "203.0.113.50".into(),
            account: Some(threats::keyed(("account", 100 + i))),
            kind: "failed",
        });
    }
    // One session from three addresses.
    for (i, ip) in ["192.0.2.10", "192.0.2.11", "192.0.2.12"]
        .iter()
        .enumerate()
    {
        let mut o = Obs::synthetic(now - 3, ip, "GET", "/account", 200, 0);
        o.session = Some(42);
        o.ua_hash = i as u64;
        t.ingest(o);
    }
    for _ in 0..25 {
        t.csp(now - 2);
    }
    let found = kinds(&t, now);
    for kind in [
        "scanner",
        "traversal",
        "sqli",
        "xss",
        "odd_method",
        "csrf",
        "rate_limited",
        "ip_rate",
        "brute_force",
        "credential_stuffing",
        "session_hop",
        "csp_spike",
    ] {
        assert!(found.contains(&kind), "{kind} missing from {found:?}");
    }
    // Ten minutes later it's all history.
    assert!(t.findings(now + threats::WINDOW_SECS + 120).is_empty());
}

/// The attack simulator, through the real router, lights up every request detector, attributes
/// each run to its TEST-NET address, and the Security page and Insights show them.
#[tokio::test]
async fn the_attack_simulator_lights_up_every_detector() {
    let app = dev_app().await;
    for (kind, _, _) in lab::ATTACKS {
        let plan = lab::attack_plan(kind).unwrap();
        // SECURITY: paths on this app only, never a URL (test: the_attack_simulator_lights_up_every_detector).
        assert!(
            plan.iter()
                .all(|r| r.path.starts_with('/') && !r.path.starts_with("//"))
        );
        assert!(plan.len() <= lab::MAX_ATTACK);
    }
    assert!(lab::attack_plan("nope").is_none());
    let router = offrails_host::routes::router(app.state.clone()).unwrap();
    // WHY judged as of the run's start: on a loaded machine the run can outlast the detectors'
    // window, and its first requests would have aged out.
    let started = chrono::Utc::now().timestamp();
    lab::run_attack(
        lab::Target::InProcess(router),
        lab::attack_plan("all").unwrap(),
    )
    .await;
    let now = started + 60;
    let found = kinds(&lock(&HUB.threats), now);
    for kind in [
        "scanner",
        "traversal",
        "sqli",
        "xss",
        "odd_method",
        "csrf",
        "brute_force",
        "credential_stuffing",
        "session_hop",
        "ua_rotation",
    ] {
        assert!(found.contains(&kind), "{kind} missing from {found:?}");
    }
    let page = app.get("/dev/dashboard/security").await;
    page.assert_ok()
        .assert_contains("Under attack")
        .assert_text("Scanner from 203.0.113.7")
        .assert_text("Credential stuffing from 203.0.113.50")
        .assert_contains("https://rustoffrails.org/docs/guides/security");
    let all = insights::findings(&app.state).await;
    assert!(
        all.iter()
            .any(|f| f.kind == "security" && f.title.starts_with("Brute force on one account"))
    );
    let out = ai_tools::call(
        &app.state,
        false,
        false,
        "list_security_findings",
        &json!({}),
    )
    .await;
    assert!(out.text.contains("scanner"), "{}", out.text);
    let id = out
        .text
        .lines()
        .find_map(|l| {
            l.split("evidence: ")
                .nth(1)?
                .split(' ')
                .next()
                .map(str::to_owned)
        })
        .filter(|id| id.len() == 12)
        .unwrap();
    let ev = ai_tools::call(
        &app.state,
        false,
        false,
        "get_security_event",
        &json!({ "id": id }),
    )
    .await;
    assert!(!ev.is_error, "{}", ev.text);
}

/// Only the simulator's own requests (this process's secret, development) may claim a client
/// address; anyone else's claim is ignored.
#[tokio::test]
async fn a_simulated_client_address_needs_the_simulators_secret() {
    let app = dev_app().await;
    app.get_with_headers(
        "/.env",
        &[
            (threats::SIM_HEADER, "not-the-secret"),
            (threats::SIM_CLIENT, "203.0.113.99"),
        ],
    )
    .await;
    let recent = lock(&HUB.threats).recent(5);
    assert!(!recent.is_empty());
    assert!(recent.iter().all(|e| e.ip != "203.0.113.99"), "{recent:?}");
    app.get_with_headers(
        "/.env",
        &[
            (threats::SIM_HEADER, threats::SIM_SECRET.as_str()),
            (threats::SIM_CLIENT, "203.0.113.99"),
        ],
    )
    .await;
    assert_eq!(lock(&HUB.threats).recent(1)[0].ip, "203.0.113.99");
}

/// Production shows a client address as its network and a keyed hash, never whole: in the
/// detectors' state, on the Security page, and to Ask AI.
#[tokio::test]
async fn client_addresses_are_truncated_in_production() {
    let v4: std::net::IpAddr = "203.0.113.77".parse().unwrap();
    let label = threats::ip_label(Some(v4), true);
    assert!(
        label.starts_with("203.0.113.x #") && !label.contains("203.0.113.77"),
        "{label}"
    );
    assert_eq!(threats::ip_label(Some(v4), false), "203.0.113.77");
    let v6: std::net::IpAddr = "2001:db8:85a3::8a2e:370:7334".parse().unwrap();
    let label = threats::ip_label(Some(v6), true);
    assert!(
        label.starts_with("2001:db8:85a3::/48 #") && !label.contains("370:7334"),
        "{label}"
    );
    let app = TestApp::with_config(production(gated())).await;
    for p in ["/.env", "/wp-login.php", "/.git/config", "/phpmyadmin"] {
        // Behind a trusted proxy the client is the rightmost X-Forwarded-For hop.
        app.get_with_headers(p, &[("x-forwarded-for", "10.0.0.1, 203.0.113.77")])
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
    // In production the simulator's header means nothing.
    app.get_with_headers(
        "/.env",
        &[
            (threats::SIM_HEADER, threats::SIM_SECRET.as_str()),
            (threats::SIM_CLIENT, "192.0.2.1"),
        ],
    )
    .await;
    let page = app
        .get_with_headers("/admin/dashboard/security", &[("authorization", &basic())])
        .await;
    page.assert_ok()
        .assert_contains("203.0.113.x #")
        .assert_not_contains("203.0.113.77")
        .assert_not_contains("192.0.2.1");
    let text = threats::list_text(true);
    assert!(
        text.contains("203.0.113.x") && !text.contains("203.0.113.77"),
        "{text}"
    );
}

/// A path or agent an attacker chose is stored without control characters and rendered escaped.
#[tokio::test]
async fn hostile_paths_and_agents_are_stored_inert() {
    let app = dev_app().await;
    assert_eq!(threats::clean_path("/a\u{1b}[2J\nb"), "/a[2Jb");
    let now = chrono::Utc::now().timestamp();
    lock(&HUB.threats).ingest(Obs::synthetic(
        now,
        "192.0.2.9",
        "GET",
        "/x?q=<script>alert('owned')</script>",
        404,
        sig::XSS,
    ));
    let ua = threats::ua_family("sqlmap/1.8 \u{1b}[31m<b>hi</b>");
    assert_eq!(ua, "sqlmap");
    let page = app.get("/dev/dashboard/security").await;
    page.assert_ok()
        .assert_not_contains("<script>alert('owned')")
        .assert_not_contains("\u{1b}");
}

/// Every new ring and map stays bounded however much arrives.
#[test]
fn the_signals_stay_within_their_memory_bound() {
    let now = 1_800_000_000;
    let mut t = Threats::default();
    for i in 0..(threats::MAX_IPS * 3) {
        let ip = format!("10.{}.{}.{}", i / 65_536, (i / 256) % 256, i % 256);
        let mut o = Obs::synthetic(now, &ip, "GET", "/.env", 404, 0);
        o.session = Some(i as u64);
        t.ingest(o);
    }
    let (events, ips, sessions, accounts) = t.lens();
    assert!(events <= threats::MAX_EVENTS && ips <= threats::MAX_IPS);
    assert!(sessions <= threats::MAX_SESSIONS && accounts <= threats::MAX_ACCOUNTS);
    let mut u = upstreams::Upstreams::default();
    for i in 0..(upstreams::MAX_UPSTREAMS * 2) {
        u.record(
            &outbound::CallRecord {
                service: "svc",
                method: "GET",
                host: "api.example.com".into(),
                route: format!("/r{i}"),
                status: Some(200),
                outcome: outbound::Outcome::Ok,
                attempts: 1,
                timeout: Some(Duration::from_secs(1)),
                dur: Duration::from_millis(5),
                bytes: 10,
            },
            None,
        );
    }
    assert_eq!(u.len(), upstreams::MAX_UPSTREAMS);
    assert_eq!(u.dropped, upstreams::MAX_UPSTREAMS as u64);
    let mut r = rum::Rum::default();
    for i in 0..(rum::MAX_CLICKS * 2) {
        let report: rum::Report = serde_json::from_value(json!({
            "v": 1, "page": format!("/p{i}"), "events": [
                { "t": "dead", "sel": format!("#b{i}") },
                { "t": "js_error", "msg": format!("boom {i}") },
                { "t": "vital", "name": "LCP", "value": 1200 }
            ]
        }))
        .unwrap();
        r.ingest(report, chrono::Utc::now());
    }
    let (clicks, errors, pages) = r.lens();
    assert!(clicks <= rum::MAX_CLICKS && errors <= rum::MAX_ERRORS && pages <= rum::MAX_PAGES);
}

// ---- the frontend -----------------------------------------------------------------------------

async fn post_rum(app: &TestApp, path: &str, body: &str, headers: &[(&str, &str)]) -> StatusCode {
    use tower::ServiceExt;
    let mut b = axum::http::Request::post(path).header("content-type", "text/plain");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let res = offrails_host::routes::router(app.state.clone())
        .unwrap()
        .oneshot(b.body(axum::body::Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    res.status()
}

const SAME: &[(&str, &str)] = &[("sec-fetch-site", "same-origin")];

/// What the beacon reports keeps no text, value or query string: unknown fields are dropped,
/// selectors with quotes or markup are refused, pages lose their query.
#[tokio::test]
async fn rum_reports_keep_no_text_values_or_queries() {
    let app = dev_app().await;
    let body = json!({
        "v": 1,
        "page": "/orders/7?token=secret-token-123&email=alice%40example.com",
        "rid": "abc-123",
        "nav": "load",
        "text": "Dear Alice, your password is hunter2",
        "events": [
            { "t": "dead", "sel": "input[value=\"hunter2\"]", "text": "hunter2" },
            { "t": "rage", "sel": "#publish-button", "n": 5, "value": "hunter2", "innerText": "Publish for alice@example.com" },
            { "t": "js_error", "msg": "Cannot read 'x' for alice@example.com at https://app.example.com/static/a.js?sig=secret-token-123:3:9", "src": "/static/a.js:3:9" }
        ]
    })
    .to_string();
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, &body, SAME).await,
        StatusCode::NO_CONTENT
    );
    let page = app.get("/dev/dashboard/frontend").await;
    page.assert_ok()
        .assert_text("#publish-button")
        .assert_text("/orders/7");
    let text = rum::list_text();
    for leak in [
        "hunter2",
        "secret-token-123",
        "alice@",
        "Dear Alice",
        "token=",
    ] {
        assert!(!page.body.contains(leak), "{leak} on the page");
        assert!(!text.contains(leak), "{leak} in the AI text:\n{text}");
    }
}

/// A reported message can't smuggle terminal escapes, fake log lines or markup in.
#[tokio::test]
async fn rum_messages_are_cleaned_before_they_are_kept() {
    let app = dev_app().await;
    let body = json!({ "v": 1, "page": "/", "events": [
        { "t": "js_error", "msg": "boom\u{1b}[2J\n2026-10-17 ERROR fake log line <img src=x onerror=alert(1)>", "stack": "Error: boom\n    at f (https://app.example.com/static/x.js?v=9:1:2)" }
    ]})
    .to_string();
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, &body, SAME).await,
        StatusCode::NO_CONTENT
    );
    let kept = rum::list_text();
    assert!(!kept.contains('\u{1b}') && !kept.contains("v=9"), "{kept}");
    assert!(
        kept.contains("boom [2J 2026-10-17 ERROR fake log line"),
        "{kept}"
    );
    let page = app.get("/dev/dashboard/frontend").await;
    page.assert_ok()
        .assert_not_contains("<img src=x")
        .assert_contains("/static/x.js:1:2");
}

/// The endpoint takes same-origin reports only, refuses oversized ones, and rate-limits each
/// client (production, where the limit isn't lifted); it doesn't exist unless the beacon is on.
#[tokio::test]
async fn the_rum_endpoint_is_same_origin_capped_and_rate_limited() {
    let app = dev_app().await;
    let ok = json!({ "v": 1, "page": "/", "events": [{ "t": "dead", "sel": "#x" }] }).to_string();
    assert_eq!(
        post_rum(
            &app,
            rum::DEV_ENDPOINT,
            &ok,
            &[("sec-fetch-site", "cross-site")]
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_rum(
            &app,
            rum::DEV_ENDPOINT,
            &ok,
            &[("origin", "https://evil.example")]
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let big = format!(
        "{{\"v\":1,\"page\":\"/\",\"events\":[],\"pad\":\"{}\"}}",
        "x".repeat(rum::MAX_BODY)
    );
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, &big, SAME).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, "not json", SAME).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, &ok, SAME).await,
        StatusCode::NO_CONTENT
    );
    // Production with the beacon off: no endpoint.
    let off = TestApp::with_config(production(gated())).await;
    assert_eq!(
        post_rum(
            &off,
            rum::PROD_ENDPOINT,
            &ok,
            &[
                ("origin", "https://app.example.com"),
                ("sec-fetch-site", "same-origin")
            ]
        )
        .await,
        StatusCode::NOT_FOUND
    );
    // On: same-origin reports, at most PER_MINUTE a minute per client.
    let on = TestApp::with_config(production(DashboardConfig {
        rum: Some(true),
        ..gated()
    }))
    .await;
    let h = [
        ("origin", "https://app.example.com"),
        ("sec-fetch-site", "same-origin"),
    ];
    for _ in 0..rum::PER_MINUTE {
        assert_eq!(
            post_rum(&on, rum::PROD_ENDPOINT, &ok, &h).await,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        post_rum(&on, rum::PROD_ENDPOINT, &ok, &h).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        post_rum(
            &on,
            rum::PROD_ENDPOINT,
            &ok,
            &[
                ("origin", "https://evil.example"),
                ("sec-fetch-site", "cross-site")
            ]
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

/// The beacon loads on app pages in development, and in production only when turned on (sampled).
#[tokio::test]
async fn the_beacon_loads_in_development_and_in_production_only_when_enabled() {
    let dev = dev_app().await;
    dev.get("/")
        .await
        .assert_ok()
        .assert_contains("/static/dashboard-rum.js?v=")
        .assert_contains("data-rum=\"/dev/dashboard/rum\"");
    let off = TestApp::with_config(production(gated())).await;
    off.get("/")
        .await
        .assert_ok()
        .assert_not_contains("dashboard-rum.js");
    let on = TestApp::with_config(production(DashboardConfig {
        rum: Some(true),
        rum_sample: Some(1.0),
        ..gated()
    }))
    .await;
    on.get("/")
        .await
        .assert_ok()
        .assert_contains("dashboard-rum.js")
        .assert_contains("data-rum=\"/rum\"");
    let none = TestApp::with_config(production(DashboardConfig {
        rum: Some(true),
        rum_sample: Some(0.0),
        ..gated()
    }))
    .await;
    none.get("/")
        .await
        .assert_ok()
        .assert_not_contains("dashboard-rum.js");
    // Without the read-only view to read them, production sends nothing.
    let ungated = TestApp::with_config(production(DashboardConfig {
        rum: Some(true),
        ..DashboardConfig::default()
    }))
    .await;
    ungated
        .get("/")
        .await
        .assert_ok()
        .assert_not_contains("dashboard-rum.js");
}

/// Rage clicks (with the slow request behind them), dead clicks after a boosted navigation, a
/// JS error after a boosted navigation and a poor LCP become insights, and the Frontend tab and
/// Ask AI's tool list them.
#[tokio::test]
async fn frontend_signals_become_insights() {
    let app = dev_app().await;
    let slow = app.get("/dev/lab/orders").await;
    let rid = slow.headers["x-request-id"].to_str().unwrap().to_owned();
    for _ in 0..3 {
        let body = json!({ "v": 1, "page": "/dev/lab/ux", "rid": rid, "nav": "load", "events": [
            { "t": "rage", "sel": "#slow-report", "n": 6, "ms": 540, "rid": rid },
            { "t": "vital", "name": "LCP", "value": 5200 },
            { "t": "vital", "name": "CLS", "value": 0.02 }
        ]})
        .to_string();
        assert_eq!(
            post_rum(&app, rum::DEV_ENDPOINT, &body, SAME).await,
            StatusCode::NO_CONTENT
        );
    }
    for _ in 0..4 {
        let body = json!({ "v": 1, "page": "/dev/lab/ux/widget", "nav": "boosted", "events": [
            { "t": "dead", "sel": "#show-code" },
            { "t": "js_error", "msg": "widget is not defined", "src": "/static/kernel/lab-widget.js:4:3" }
        ]})
        .to_string();
        post_rum(&app, rum::DEV_ENDPOINT, &body, SAME).await;
    }
    let found = rum::findings();
    let titles: Vec<&str> = found.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles
            .iter()
            .any(|t| t
                .starts_with("Rage clicks on #slow-report on /dev/lab/ux (18 clicks in 3 bursts)")),
        "{titles:?}"
    );
    assert!(
        titles
            .contains(&"Dead clicks on #show-code on /dev/lab/ux/widget after boosted navigation"),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("JS error after boosted navigation: widget is not defined")),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("Poor LCP on /dev/lab/ux")),
        "{titles:?}"
    );
    let dead = found
        .iter()
        .find(|f| f.title.starts_with("Dead clicks"))
        .unwrap();
    assert!(dead.fix.contains("htmx:load"), "{}", dead.fix);
    app.get("/dev/dashboard/frontend")
        .await
        .assert_ok()
        .assert_text("#show-code")
        .assert_text("widget is not defined")
        .assert_contains(&format!("/dev/dashboard/traces?rid={rid}"));
    app.get(&format!("/dev/dashboard/traces?rid={rid}"))
        .await
        .assert_ok()
        .assert_contains("/dev/lab/orders");
    let out = ai_tools::call(&app.state, false, false, "list_frontend_issues", &json!({})).await;
    assert!(
        out.text.contains("rage clicks on `#slow-report`"),
        "{}",
        out.text
    );
}

/// The lab's UX pages: the dead widget's script binds on DOMContentLoaded from the body (the
/// boosted-navigation mistake), the simulation flag, the lab sign-in form's limits.
#[tokio::test]
async fn the_ux_lab_and_the_lab_sign_in_behave() {
    let app = dev_app().await;
    app.get("/dev/lab/ux?simulate=1")
        .await
        .assert_ok()
        .assert_contains("data-lab-simulate")
        .assert_contains("id=\"save-draft\"")
        .assert_contains("/static/kernel/lab-ux.js?v=")
        .assert_contains("/static/dashboard-rum.js?v=");
    app.get("/dev/lab/ux/widget")
        .await
        .assert_ok()
        .assert_contains("/static/kernel/lab-widget.js?v=");
    let js = std::fs::read_to_string("static/kernel/lab-widget.js").unwrap();
    assert!(js.contains("DOMContentLoaded"));
    for i in 0..lab::LAB_ACCOUNT_LIMIT {
        app.post_form(
            "/dev/lab/sign-in",
            &[
                ("email", "victim@example.com"),
                ("password", &format!("g{i}")),
            ],
        )
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    app.post_form(
        "/dev/lab/sign-in",
        &[("email", "victim@example.com"), ("password", "g")],
    )
    .await
    .assert_status(StatusCode::TOO_MANY_REQUESTS);
}

/// The waterfall's SQL labels read well for aggregates over subqueries.
#[test]
fn sql_labels_handle_counts_over_subqueries() {
    assert_eq!(
        waterfall::sql_summary(
            "SELECT COUNT(*) AS num_items FROM (SELECT \"lab_orders\".\"id\" FROM \"lab_orders\") AS \"sub_query\""
        ),
        "COUNT lab_orders"
    );
    assert_eq!(
        waterfall::sql_summary(
            "SELECT \"a\".\"id\" FROM \"lab_orders\" WHERE \"lab_orders\".\"id\" = ?"
        ),
        "SELECT lab_orders WHERE id = ?"
    );
}

fn span(
    id: u64,
    parent: Option<u64>,
    name: &'static str,
    start_us: u64,
    dur_us: u64,
    fields: &str,
) -> offrails_host::kernel::dashboard::trace::SpanRec {
    offrails_host::kernel::dashboard::trace::SpanRec {
        id,
        parent,
        name,
        target: "offrails_host::kernel::outbound",
        file: None,
        line: None,
        start_us,
        dur_us,
        fields: fields.to_owned(),
    }
}

fn request(
    id: u64,
    dur_us: u64,
    spans: Vec<offrails_host::kernel::dashboard::trace::SpanRec>,
) -> Arc<offrails_host::kernel::dashboard::trace::Trace> {
    Arc::new(offrails_host::kernel::dashboard::trace::Trace {
        id,
        at: chrono::Utc::now(),
        kind: "request",
        from_disk: false,
        method: "GET",
        route: "/dev/lab/checkout",
        path: "/dev/lab/checkout".into(),
        status: 200,
        dur_us,
        bytes: None,
        request_id: format!("req-{id}"),
        spans,
        sql: Vec::new(),
        events: Vec::new(),
        n_plus_one: Vec::new(),
        statements: 0,
        sql_us: 0,
        dropped: 0,
    })
}

/// A call's retries run one after another by nature: its attempts are never a "sequential calls"
/// run, while two separate calls one after another still are.
#[test]
fn retry_attempts_are_not_sequential_calls() {
    let call = "service=\"shipping\" http.method=\"POST\" http.host=api.example.com http.route=/rates attempts=2";
    let retried = request(
        1,
        600_000,
        vec![
            span(1, None, "handler", 0, 590_000, "handler=\"lab::checkout\""),
            span(2, Some(1), "http", 1_000, 500_000, call),
            span(
                3,
                Some(2),
                "http_attempt",
                1_000,
                250_000,
                "attempt=1 outcome=\"timeout\"",
            ),
            span(
                4,
                Some(2),
                "http_attempt",
                251_000,
                249_000,
                "attempt=2 http.status=200 outcome=\"ok\"",
            ),
        ],
    );
    let root = waterfall::tree(&retried, true);
    assert!(
        upstreams::sequential(&root).is_none(),
        "{:?}",
        upstreams::sequential(&root)
    );
    assert_eq!(upstreams::concurrent(&root), 1);
    // Two calls of their own, one after another: still flagged.
    let two_calls = || {
        vec![
            span(1, None, "handler", 0, 590_000, "handler=\"lab::checkout\""),
            span(2, Some(1), "http", 1_000, 250_000, call),
            span(
                5,
                Some(1),
                "http",
                252_000,
                240_000,
                "service=\"payments\" http.method=\"POST\" http.host=api.example.com http.route=/charge",
            ),
        ]
    };
    let two = request(2, 600_000, two_calls());
    let s = upstreams::sequential(&waterfall::tree(&two, true)).unwrap();
    assert_eq!(s.calls.len(), 2);
    // Work of the handler's between them (the second call needs it): not a run, however short.
    let mut spans = two_calls();
    spans.push(span(6, Some(1), "load_cart", 251_200, 300, ""));
    let worked = request(3, 600_000, spans);
    assert!(upstreams::sequential(&waterfall::tree(&worked, true)).is_none());
}

/// The time between a request's arrival and its first code is a `wait` row and its own share of
/// the time-by-kind bar; several requests that waited long become a saturation insight.
#[tokio::test]
async fn queue_time_is_its_own_kind_and_an_insight() {
    let app = dev_app().await;
    let waited = |id| {
        request(
            id,
            1_000_000,
            vec![span(
                1,
                None,
                "handler",
                700_000,
                290_000,
                "handler=\"lab::checkout\"",
            )],
        )
    };
    let t = waited(10);
    let root = waterfall::tree(&t, true);
    assert_eq!(waterfall::waited_us(&root), 700_000);
    let split = waterfall::time_by_kind(&root);
    assert_eq!(split[0], ("wait", 700_000), "{split:?}");
    let view = waterfall::view(
        &t,
        true,
        "/dev/dashboard",
        false,
        true,
        false,
        String::new(),
    );
    assert!(
        view.rows
            .iter()
            .any(|r| r.kind == "wait" && r.label == "waiting to run")
    );
    assert!(
        view.split
            .iter()
            .any(|(k, label, ..)| *k == "wait" && *label == "waiting (queue)")
    );
    // A request that started at once has no wait row.
    let quick = request(
        11,
        300_000,
        vec![span(1, None, "handler", 200, 290_000, "")],
    );
    assert_eq!(waterfall::waited_us(&waterfall::tree(&quick, true)), 0);
    for id in 20..23 {
        lock(&HUB.traces).push(waited(id));
    }
    let found = insights::findings(&app.state).await;
    let f = found
        .iter()
        .find(|f| f.kind == "saturation")
        .unwrap_or_else(|| panic!("{:?}", found.iter().map(|f| &f.title).collect::<Vec<_>>()));
    assert!(
        f.title
            .starts_with("Requests waited up to 700ms before running"),
        "{}",
        f.title
    );
    assert!(
        f.evidence.starts_with("3 requests waited"),
        "{}",
        f.evidence
    );
}

/// Counts read right: "1 error", "2 errors", "0 errors", on the Frontend tab's badges too.
#[tokio::test]
async fn counts_are_pluralized() {
    use offrails_host::kernel::dashboard::web::count;
    assert_eq!(count(1, "error", "errors"), "1 error");
    assert_eq!(count(2_u64, "error", "errors"), "2 errors");
    assert_eq!(count(0_usize, "error", "errors"), "0 errors");
    let app = dev_app().await;
    let body =
        json!({ "v": 1, "page": "/", "events": [{ "t": "js_error", "msg": "boom" }] }).to_string();
    assert_eq!(
        post_rum(&app, rum::DEV_ENDPOINT, &body, SAME).await,
        StatusCode::NO_CONTENT
    );
    app.get("/dev/dashboard/frontend")
        .await
        .assert_ok()
        .assert_contains(">1 error<")
        .assert_contains(">0 rage clicks<")
        .assert_not_contains("1 errors");
}
