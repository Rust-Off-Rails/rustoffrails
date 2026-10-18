//! `/admin/jobs`: access, every page and fragment, actions with their confirmations, escaping
//! and redaction, caching, CSS/JS loading. Runs on SQLite and Postgres.
//! GUIDE: .offrails/guides/jobs-dashboard.md

use std::sync::Arc;

use axum::http::StatusCode;
use base64::Engine;
use chrono::Utc;
use offrails_host::{
    config::{AppEnv, Config, JobsDashboardConfig},
    kernel::jobs::{
        DispatchFn, JobBuilder, JobDefaults, JobState, Outcome, WorkerConfig, admin, executor,
    },
    routes::{Access, ROUTES},
};
use sea_orm::DatabaseConnection;
use serde_json::{Value, json};

use crate::{security_test::concrete, support::TestApp};

const PASSWORD: &str = "correct horse battery staple";

fn development() -> Config {
    Config {
        env: AppEnv::Development,
        ..Config::test()
    }
}

fn production(password: Option<&str>) -> Config {
    Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        jobs_dashboard: JobsDashboardConfig::new(password.map(str::to_owned)).unwrap(),
        ..Config::test()
    }
}

fn basic(password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("admin:{password}"))
    )
}

fn builder(worker: &'static str, args: Value) -> JobBuilder {
    JobBuilder::new(
        JobDefaults {
            worker,
            queue: "default",
            max_attempts: 2,
            priority: 0,
            unique: None,
        },
        Ok(args),
    )
}

fn dispatcher() -> DispatchFn {
    Arc::new(|worker, _args, _job| {
        Box::pin(async move {
            match worker.as_str() {
                "fail" => Err(anyhow::anyhow!("connection refused").context("charging card 42")),
                "discard" => Ok(Outcome::Discard("invalid address <b>bold</b>".into())),
                _ => Ok(Outcome::Done),
            }
        })
    })
}

fn config(_: &str) -> WorkerConfig {
    WorkerConfig::default()
}

async fn drain(db: &DatabaseConnection) {
    executor::drain(db, &dispatcher(), config, None, false)
        .await
        .unwrap();
}

async fn insert(db: &DatabaseConnection, b: JobBuilder) -> i64 {
    b.insert(db).await.unwrap().id
}

async fn state_of(db: &DatabaseConnection, id: i64) -> Option<JobState> {
    admin::get(db, id).await.unwrap().map(|j| j.state)
}

/// A few jobs in several states, for the pages to show.
async fn seed(db: &DatabaseConnection) -> (i64, i64, i64) {
    for i in 0..3 {
        insert(db, builder("export_report", json!({"report_id": i}))).await;
    }
    let failed = insert(
        db,
        builder("fail", json!({"order_id": 42})).tags(["user:7", "billing"]),
    )
    .await;
    let discarded = insert(db, builder("discard", json!({"email": "x"}))).await;
    drain(db).await;
    let waiting = insert(
        db,
        builder("export_report", json!({"report_id": 9})).schedule_in(chrono::Duration::hours(2)),
    )
    .await;
    (failed, discarded, waiting)
}

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

/// Security: production without an admin gate has no dashboard at all.
#[tokio::test]
async fn production_without_an_admin_gate_is_404() {
    let app = TestApp::with_config(production(None)).await;
    for (method, path, handler, access) in ROUTES {
        if *access != Access::Admin || !path.starts_with("/admin/jobs") {
            continue;
        }
        let status = match *method {
            "GET" => app.get(&concrete(path)).await.status,
            _ => app.post_form(&concrete(path), &[]).await.status,
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path} ({handler})");
    }
}

/// Security: with APP_ADMIN_PASSWORD, every route needs it; nothing answers 2xx without it.
#[tokio::test]
async fn the_admin_password_gates_every_dashboard_route() {
    let app = TestApp::with_config(production(Some(PASSWORD))).await;
    let mut checked = 0;
    for (method, path, handler, access) in ROUTES {
        if *access != Access::Admin || !path.starts_with("/admin/jobs") {
            continue;
        }
        checked += 1;
        let res = match *method {
            "GET" => app.get(&concrete(path)).await,
            _ => app.post_form(&concrete(path), &[]).await,
        };
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} ({handler})"
        );
        assert!(
            res.headers
                .get("www-authenticate")
                .is_some_and(|v| v.to_str().unwrap().starts_with("Basic")),
            "{path} asks for credentials"
        );
    }
    assert_eq!(checked, 11);

    let wrong = app
        .get_with_headers(
            "/admin/jobs",
            &[("authorization", &basic("not the password!!"))],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let right = app
        .get_with_headers("/admin/jobs", &[("authorization", &basic(PASSWORD))])
        .await;
    right.assert_ok();
    // Production shows no development tabs.
    right.assert_not_contains("href=\"/dev");
}

/// Security: guessing stops after 10 failed passwords, even for the right one.
#[tokio::test]
async fn failed_admin_passwords_are_rate_limited() {
    let app = TestApp::with_config(production(Some(PASSWORD))).await;
    // The browser's first, credential-less request doesn't count.
    for _ in 0..20 {
        assert_eq!(
            app.get("/admin/jobs").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    for i in 0..10 {
        let res = app
            .get_with_headers(
                "/admin/jobs",
                &[("authorization", &basic(&format!("guess number {i:04}")))],
            )
            .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "try {i}");
    }
    let res = app
        .get_with_headers("/admin/jobs", &[("authorization", &basic(PASSWORD))])
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn a_short_admin_password_is_refused() {
    assert!(JobsDashboardConfig::new(Some("short".into())).is_err());
    assert!(JobsDashboardConfig::new(Some(PASSWORD.into())).is_ok());
    assert!(JobsDashboardConfig::new(None).is_ok());
    let c = JobsDashboardConfig::new(Some(PASSWORD.into())).unwrap();
    assert!(!format!("{c:?}").contains("horse"), "Debug redacts it");
}

/// Security: a page on another site can't make an admin's browser change jobs.
#[tokio::test]
async fn cross_site_dashboard_posts_are_rejected() {
    let app = TestApp::with_config(development()).await;
    let (failed, ..) = seed(app.db()).await;
    let path = format!("/admin/jobs/{failed}/retry");
    let res = app
        .post_form_with_headers(&path, &[], &[("sec-fetch-site", "cross-site")])
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    app.without_fetch_metadata()
        .with_origin("https://evil.example");
    assert_eq!(
        app.post_form(&path, &[]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(state_of(app.db(), failed).await, Some(JobState::Retryable));
}

/// Security: GET never changes anything; actions are POST only.
#[tokio::test]
async fn get_never_changes_anything() {
    let app = TestApp::with_config(development()).await;
    let (failed, ..) = seed(app.db()).await;
    for path in [
        format!("/admin/jobs/{failed}/delete"),
        format!("/admin/jobs/{failed}/retry"),
        "/admin/jobs/bulk?action=delete&all_matching=1&state=retryable&confirmed=1".into(),
        "/admin/jobs/queues/default/pause".into(),
        "/admin/jobs/queues/pause-all".into(),
        "/admin/jobs/cron/cleanup/run".into(),
    ] {
        let res = app.get(&path).await;
        assert!(
            matches!(
                res.status,
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
            ),
            "GET {path}: {}",
            res.status
        );
    }
    assert_eq!(state_of(app.db(), failed).await, Some(JobState::Retryable));
    let queues = admin::queues(app.db(), Utc::now()).await.unwrap();
    assert!(queues.iter().all(|q| !q.paused));
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_overview_shows_counts_charts_queues_workers_and_nodes() {
    let app = TestApp::with_config(development()).await;
    let empty = app.get("/admin/jobs").await;
    empty
        .assert_ok()
        .assert_text("No job finished in the last hour")
        .assert_text("Nothing is running right now")
        .assert_text("No worker has checked in");
    seed(app.db()).await;
    admin::pause(app.db(), "mailers").await.unwrap();
    let res = app.get("/admin/jobs").await;
    res.assert_ok()
        .assert_contains("href=\"/admin/jobs/list?state=retryable\"")
        .assert_text("Throughput")
        .assert_text("Queue wait")
        .assert_contains("class=\"chart\"")
        .assert_contains("<title>")
        .assert_text("paused")
        .assert_text("no nodes")
        .assert_contains("action=\"/admin/jobs/queues/mailers/resume\"")
        .assert_text("export_report")
        .assert_contains("<meter")
        .assert_contains("hx-trigger=\"every 2s\"");
    // Live off: no polling at all.
    let frozen = app.get("/admin/jobs?live=0").await;
    frozen
        .assert_ok()
        .assert_not_contains("hx-trigger")
        .assert_text("Paused");
}

/// The legend sums up what the chart shows: queue wait p95 and p50 over the last 60 minutes, not
/// the last minute (usually empty, which read "p95 0ms" beside a 8.1 s peak), and "—" with no
/// data at all.
#[tokio::test]
async fn chart_legends_summarize_the_whole_window() {
    use offrails_host::kernel::jobs::job;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};

    let app = TestApp::with_config(development()).await;
    app.get_htmx("/admin/jobs?part=charts")
        .await
        .assert_ok()
        .assert_contains("p95 <strong>—</strong>")
        .assert_contains("p50 <strong>—</strong>")
        .assert_contains("Completed <strong>0</strong>");

    // Eight jobs started 20 minutes ago that waited 1 s … 8 s; nothing in the last minute.
    let started = Utc::now() - chrono::Duration::minutes(20);
    for wait_s in 1..=8_i64 {
        let id = insert(
            app.db(),
            builder("export_report", json!({"report_id": wait_s})),
        )
        .await;
        job::Entity::update_many()
            .col_expr(job::Column::State, Expr::value(JobState::Completed))
            .col_expr(job::Column::AttemptedAt, Expr::value(started))
            .col_expr(job::Column::CompletedAt, Expr::value(started))
            .col_expr(
                job::Column::ScheduledAt,
                Expr::value(started - chrono::Duration::seconds(wait_s)),
            )
            .filter(job::Column::Id.eq(id))
            .exec(app.db())
            .await
            .unwrap();
    }
    let window = admin::throughput(app.db(), Utc::now(), 60).await.unwrap();
    assert_eq!(window.buckets.last().unwrap().wait_p95_ms, None);
    assert_eq!(
        (window.wait_p50_ms, window.wait_p95_ms),
        (Some(4_000), Some(8_000))
    );
    app.get_htmx("/admin/jobs?part=charts")
        .await
        .assert_ok()
        .assert_contains("p95 <strong>8.0s</strong>")
        .assert_contains("p50 <strong>4.0s</strong>")
        .assert_contains("worst p95 8.0s")
        .assert_contains("Completed <strong>8</strong>");
}

#[tokio::test]
async fn every_fragment_renders_alone() {
    let app = TestApp::with_config(development()).await;
    seed(app.db()).await;
    for (part, id) in [
        ("stats", "id=\"stats\""),
        ("charts", "id=\"charts\""),
        ("running", "id=\"running\""),
        ("queues", "id=\"queues\""),
        ("workers", "id=\"workers\""),
        ("nodes", "id=\"nodes\""),
    ] {
        let res = app.get_htmx(&format!("/admin/jobs?part={part}")).await;
        res.assert_ok()
            .assert_contains(id)
            .assert_not_contains("<html");
    }
    app.get_htmx("/admin/jobs?part=nope")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Without HX-Request a part is the whole page (a reload of a polled URL).
    app.get("/admin/jobs?part=stats")
        .await
        .assert_ok()
        .assert_contains("<html");
}

#[tokio::test]
async fn the_job_list_filters_searches_and_pages() {
    let app = TestApp::with_config(development()).await;
    let (failed, discarded, _) = seed(app.db()).await;
    let res = app.get("/admin/jobs/list").await;
    res.assert_ok()
        .assert_text("All")
        .assert_contains(&format!("href=\"/admin/jobs/{failed}\""))
        .assert_contains("hx-get=\"/admin/jobs/list?part=rows\"")
        .assert_contains("id=\"sel-");
    let res = app.get("/admin/jobs/list?state=retryable").await;
    res.assert_ok()
        .assert_contains(&format!("/admin/jobs/{failed}\""))
        .assert_not_contains(&format!("/admin/jobs/{discarded}\""))
        .assert_text("Retry all 1 matching");
    app.get("/admin/jobs/list?tag=user:7")
        .await
        .assert_contains(&format!("/admin/jobs/{failed}\""));
    app.get("/admin/jobs/list?q=order_id")
        .await
        .assert_contains(&format!("/admin/jobs/{failed}\""))
        .assert_text("Search:");
    app.get("/admin/jobs/list?error=connection+refused")
        .await
        .assert_contains(&format!("/admin/jobs/{failed}\""));
    app.get("/admin/jobs/list?worker=nobody")
        .await
        .assert_text("No jobs match this filter");
    // Bad input is ignored, never a 500.
    app.get("/admin/jobs/list?state=bogus&before=xyz&q=%27%3B--")
        .await
        .assert_ok();
    // The rows fragment.
    app.get_htmx("/admin/jobs/list?state=retryable&part=rows")
        .await
        .assert_ok()
        .assert_contains("<tbody id=\"rows\"")
        .assert_not_contains("<html");

    // Keyset paging: 60 jobs, 50 per page.
    for i in 0..60 {
        insert(app.db(), builder("export_report", json!({"page": i}))).await;
    }
    let first = app.get("/admin/jobs/list?state=available").await;
    first.assert_ok().assert_text("Older →");
    let older = first
        .body
        .split("href=\"")
        .find(|s| s.contains("before="))
        .and_then(|s| s.split('"').next())
        .unwrap()
        .replace("&#38;", "&");
    let page2 = app.get(&older).await;
    page2
        .assert_ok()
        .assert_text("← Newest")
        .assert_not_contains("hx-get=\"/admin/jobs/list?state=available&amp;part=rows\"");
}

#[tokio::test]
async fn the_job_page_shows_args_errors_per_attempt_and_timeline() {
    let app = TestApp::with_config(development()).await;
    let (failed, ..) = seed(app.db()).await;
    let res = app.get(&format!("/admin/jobs/{failed}")).await;
    res.assert_ok()
        .assert_text(&format!("Job {failed}"))
        .assert_text("Retryable")
        .assert_text("\"order_id\": 42")
        .assert_text("charging card 42: connection refused")
        .assert_text("Full report (causes and backtrace)")
        .assert_text("Attempt 1 failed")
        .assert_text("Inserted")
        .assert_text("Retries")
        .assert_contains(&format!("action=\"/admin/jobs/{failed}/run\""))
        .assert_contains(&format!("action=\"/admin/jobs/{failed}/cancel\""))
        .assert_not_contains(
            &format!("action=\"/admin/jobs/{failed}/delete\"").replace("delete", "retry"),
        )
        .assert_text(&format!("app jobs run {failed}"))
        .assert_contains("href=\"/admin/jobs/list?tag=user%3A7\"");
    // A completed job offers retry and delete, nothing else.
    let done = admin::list(
        app.db(),
        &admin::JobFilter {
            state: Some(JobState::Completed),
            ..Default::default()
        },
        None,
        1,
    )
    .await
    .unwrap()[0]
        .id;
    app.get(&format!("/admin/jobs/{done}"))
        .await
        .assert_contains(&format!("/admin/jobs/{done}/retry"))
        .assert_contains(&format!("/admin/jobs/{done}/delete"))
        .assert_not_contains(&format!("/admin/jobs/{done}/cancel"))
        .assert_text("Completed");
    app.get_htmx(&format!("/admin/jobs/{failed}?part=status"))
        .await
        .assert_ok()
        .assert_contains("id=\"job-status\"");
    app.get("/admin/jobs/not-a-number")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_deleted_jobs_page_redirects_to_the_list_with_a_flash() {
    let app = TestApp::with_config(development()).await;
    let res = app.get("/admin/jobs/987654").await;
    res.assert_redirect_to("/admin/jobs/list");
    app.get("/admin/jobs/list")
        .await
        .assert_text("Job 987654 no longer exists (deleted or pruned).");
}

/// Security: job data is escaped on every page, and sensitive args are redacted.
#[tokio::test]
async fn job_data_is_escaped_and_redacted_everywhere() {
    let app = TestApp::with_config(development()).await;
    let secret = format!("FAKE-{}", uuid::Uuid::new_v4().simple());
    let id = insert(
        app.db(),
        builder(
            "discard",
            json!({"note": "<script>alert(1)</script>", "api_key": secret, "nested": {"password": secret}}),
        )
        .tag("<img src=x onerror=alert(1)>"),
    )
    .await;
    drain(app.db()).await;
    for path in [
        "/admin/jobs".to_owned(),
        "/admin/jobs/list".to_owned(),
        "/admin/jobs/list?q=script".to_owned(),
        format!("/admin/jobs/{id}"),
        "/admin/jobs/errors".to_owned(),
    ] {
        let res = app.get(&path).await;
        res.assert_ok();
        assert!(
            !res.body.contains("<script>alert"),
            "{path} rendered a raw script"
        );
        assert!(
            !res.body.contains("<img src=x"),
            "{path} rendered a raw tag"
        );
        assert!(!res.body.contains("<b>bold"), "{path} rendered a raw error");
        assert!(!res.body.contains(&secret), "{path} leaked a sensitive arg");
    }
    app.get(&format!("/admin/jobs/{id}"))
        .await
        .assert_text("[redacted]")
        .assert_text("invalid address <b>bold</b>");
}

#[tokio::test]
async fn the_errors_page_groups_failures_and_offers_a_bulk_retry() {
    let app = TestApp::with_config(development()).await;
    app.get("/admin/jobs/errors")
        .await
        .assert_ok()
        .assert_text("Nothing is failing");
    for order in [1, 2, 3] {
        insert(
            app.db(),
            builder("fail", json!({"order": order})).max_attempts(1),
        )
        .await;
    }
    drain(app.db()).await;
    let res = app.get("/admin/jobs/errors").await;
    res.assert_ok()
        .assert_text("connection refused")
        .assert_contains(">3<")
        .assert_contains("worker=fail&#38;error=connection%20refused")
        .assert_text("Retry discarded");
    // Its bulk retry goes through the confirmation like every bulk action.
    let confirm = app
        .post_form(
            "/admin/jobs/bulk",
            &[
                ("action", "retry"),
                ("all_matching", "1"),
                ("state", "discarded"),
                ("worker", "fail"),
                ("error", "connection refused"),
                ("return_to", "/admin/jobs/errors"),
            ],
        )
        .await;
    confirm.assert_ok().assert_text("Retry 3 jobs?");
}

#[tokio::test]
async fn the_cron_page_lists_next_runs_and_runs_an_entry_now() {
    let app = TestApp::with_config(development()).await;
    let res = app.get("/admin/jobs/cron").await;
    res.assert_ok()
        .assert_text("cleanup")
        .assert_text("0 * * * *")
        .assert_text("UTC")
        .assert_text("never")
        .assert_contains("action=\"/admin/jobs/cron/cleanup/run\"");
    let res = app.post_form("/admin/jobs/cron/cleanup/run", &[]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let to = res.location().unwrap().to_owned();
    assert!(to.starts_with("/admin/jobs/"), "{to}");
    app.get(&to).await.assert_ok().assert_text("cleanup");
    app.get("/admin/jobs/cron")
        .await
        .assert_contains(&format!("href=\"{to}\""));
    app.post_form("/admin/jobs/cron/no_such_entry/run", &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_job_actions_post_redirect_and_confirm_when_destructive() {
    let app = TestApp::with_config(development()).await;
    let (failed, discarded, waiting) = seed(app.db()).await;

    // Run now and retry act at once.
    let res = app
        .post_form(&format!("/admin/jobs/{waiting}/run"), &[])
        .await;
    res.assert_redirect_to(&format!("/admin/jobs/{waiting}"));
    assert_eq!(res.headers.get("x-jobs-changed").unwrap(), "1");
    assert_eq!(state_of(app.db(), waiting).await, Some(JobState::Available));
    app.post_form(
        &format!("/admin/jobs/{discarded}/retry"),
        &[("return_to", "/admin/jobs/list")],
    )
    .await
    .assert_redirect_to("/admin/jobs/list");
    app.get("/admin/jobs/list")
        .await
        .assert_text(&format!("Retried job {discarded}."));

    // Cancel, discard and delete ask first.
    for action in ["cancel", "discard", "delete"] {
        let res = app
            .post_form(&format!("/admin/jobs/{failed}/{action}"), &[])
            .await;
        res.assert_ok()
            .assert_contains("name=\"confirmed\" value=\"1\"")
            .assert_contains(&format!("action=\"/admin/jobs/{failed}/{action}\""));
        assert_eq!(state_of(app.db(), failed).await, Some(JobState::Retryable));
    }
    app.post_form(
        &format!("/admin/jobs/{failed}/discard"),
        &[("confirmed", "1")],
    )
    .await
    .assert_redirect_to(&format!("/admin/jobs/{failed}"));
    assert_eq!(state_of(app.db(), failed).await, Some(JobState::Discarded));
    // An action the state doesn't allow changes nothing, and says so.
    app.post_form(
        &format!("/admin/jobs/{failed}/cancel"),
        &[("confirmed", "1")],
    )
    .await
    .assert_redirect_to(&format!("/admin/jobs/{failed}"));
    app.get(&format!("/admin/jobs/{failed}"))
        .await
        .assert_text("Nothing changed: the job's state doesn't allow that.");
    app.post_form(
        &format!("/admin/jobs/{failed}/delete"),
        &[("confirmed", "1")],
    )
    .await
    .assert_redirect_to("/admin/jobs/list");
    assert_eq!(state_of(app.db(), failed).await, None);

    app.post_form(&format!("/admin/jobs/{waiting}/explode"), &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// Security: redirects after an action stay on the dashboard.
#[tokio::test]
async fn actions_never_redirect_off_the_dashboard() {
    let app = TestApp::with_config(development()).await;
    let (_, discarded, _) = seed(app.db()).await;
    for evil in [
        "https://evil.example/",
        "//evil.example",
        "/admin/jobsevil",
        "/",
    ] {
        let res = app
            .post_form(
                &format!("/admin/jobs/{discarded}/retry"),
                &[("return_to", evil)],
            )
            .await;
        res.assert_redirect_to(&format!("/admin/jobs/{discarded}"));
    }
}

#[tokio::test]
async fn bulk_actions_confirm_with_the_exact_count_and_refuse_an_empty_filter() {
    let app = TestApp::with_config(development()).await;
    for i in 0..4 {
        insert(app.db(), builder("discard", json!({"i": i}))).await;
    }
    drain(app.db()).await;

    // Selected ids: confirm, then act.
    let ids: Vec<String> = admin::list(app.db(), &Default::default(), None, 10)
        .await
        .unwrap()
        .iter()
        .map(|j| j.id.to_string())
        .collect();
    let picked = [
        ("action", "retry"),
        ("ids", ids[0].as_str()),
        ("ids", ids[1].as_str()),
    ];
    app.post_form("/admin/jobs/bulk", &picked)
        .await
        .assert_ok()
        .assert_text("Retry 2 jobs?")
        .assert_text("The selected jobs");
    let mut confirmed = picked.to_vec();
    confirmed.push(("confirmed", "1"));
    let res = app.post_form("/admin/jobs/bulk", &confirmed).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.headers.get("x-jobs-changed").unwrap(), "2");

    // All matching: the action rides in the query string; the count is exact.
    let res = app
        .post_form(
            "/admin/jobs/bulk?action=delete",
            &[("all_matching", "1"), ("state", "discarded")],
        )
        .await;
    res.assert_ok()
        .assert_text("Delete 2 jobs?")
        .assert_text("state “discarded”");
    let res = app
        .post_form(
            "/admin/jobs/bulk?action=delete",
            &[
                ("all_matching", "1"),
                ("state", "discarded"),
                ("confirmed", "1"),
            ],
        )
        .await;
    assert_eq!(res.headers.get("x-jobs-changed").unwrap(), "2");
    assert_eq!(
        admin::stats(app.db())
            .await
            .unwrap()
            .count(JobState::Discarded),
        0
    );

    // An empty filter is refused outright, confirmed or not.
    let total = admin::stats(app.db()).await.unwrap().total();
    for form in [
        vec![("action", "delete"), ("all_matching", "1")],
        vec![
            ("action", "delete"),
            ("all_matching", "1"),
            ("confirmed", "1"),
        ],
    ] {
        let res = app.post_form("/admin/jobs/bulk", &form).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER);
        assert_eq!(res.headers.get("x-jobs-changed").unwrap(), "0");
    }
    assert_eq!(admin::stats(app.db()).await.unwrap().total(), total);
    // Nothing selected, or no action: a flash, no change.
    app.post_form("/admin/jobs/bulk", &[("action", "retry")])
        .await
        .assert_status(StatusCode::SEE_OTHER);
    app.post_form("/admin/jobs/bulk", &[("ids", ids[2].as_str())])
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn queues_pause_and_resume_and_pause_all_asks_first() {
    let app = TestApp::with_config(development()).await;
    seed(app.db()).await;
    app.post_form("/admin/jobs/queues/default/pause", &[])
        .await
        .assert_redirect_to("/admin/jobs");
    assert!(admin::queues(app.db(), Utc::now()).await.unwrap()[0].paused);
    app.get("/admin/jobs")
        .await
        .assert_text("Paused queue default on every node.");
    app.post_form("/admin/jobs/queues/default/resume", &[])
        .await
        .assert_redirect_to("/admin/jobs");
    app.post_form("/admin/jobs/queues/default/explode", &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.post_form("/admin/jobs/queues/Bad%20Name/pause", &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.post_form("/admin/jobs/queues/pause-all", &[])
        .await
        .assert_ok()
        .assert_text("Pause all 1 queue?");
    app.post_form("/admin/jobs/queues/pause-all", &[("confirmed", "1")])
        .await
        .assert_redirect_to("/admin/jobs");
    app.get("/admin/jobs")
        .await
        .assert_text("All queues are paused.")
        .assert_contains("action=\"/admin/jobs/queues/resume-all\"");
    app.post_form("/admin/jobs/queues/resume-all", &[])
        .await
        .assert_redirect_to("/admin/jobs");
    assert!(
        admin::queues(app.db(), Utc::now())
            .await
            .unwrap()
            .iter()
            .all(|q| !q.paused)
    );
}

// ---------------------------------------------------------------------------
// Caching, assets, performance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_dashboard_response_is_no_store() {
    let app = TestApp::with_config(development()).await;
    let (failed, ..) = seed(app.db()).await;
    let mut responses = Vec::new();
    for path in [
        "/admin/jobs".to_owned(),
        "/admin/jobs/list".to_owned(),
        "/admin/jobs/errors".to_owned(),
        "/admin/jobs/cron".to_owned(),
        format!("/admin/jobs/{failed}"),
        "/admin/jobs/424242".to_owned(),
    ] {
        responses.push((path.clone(), app.get(&path).await));
    }
    responses.push((
        "fragment".into(),
        app.get_htmx("/admin/jobs?part=stats").await,
    ));
    responses.push((
        "action".into(),
        app.post_form(&format!("/admin/jobs/{failed}/run"), &[])
            .await,
    ));
    for (path, res) in responses {
        assert_eq!(
            res.headers
                .get("cache-control")
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{path}"
        );
    }
}

/// The page links its own stylesheet and script, both are served, and every class the
/// templates use has a rule (a dashboard whose CSS never loads is a known failure elsewhere).
/// The dashboard follows the kernel header's theme switch on every page, the confirmation page
/// included (its no-JS switch returns to where Cancel goes, never to the POST URL).
#[tokio::test]
async fn the_dashboard_follows_the_theme_switch() {
    let app = TestApp::with_config(development()).await;
    let (id, _, _) = seed(app.db()).await;
    app.set_cookie("offrails_theme", "dark");
    for path in [
        "/admin/jobs".to_owned(),
        "/admin/jobs/list".to_owned(),
        "/admin/jobs/errors".to_owned(),
        "/admin/jobs/cron".to_owned(),
        format!("/admin/jobs/{id}"),
    ] {
        app.get(&path)
            .await
            .assert_ok()
            .assert_contains(r#"<html lang="en" data-theme="dark">"#);
    }
    app.post_form(&format!("/admin/jobs/{id}/delete"), &[])
        .await
        .assert_ok()
        .assert_contains(r#"<html lang="en" data-theme="dark">"#)
        .assert_contains(&format!(r#"name="return_to" value="/admin/jobs/{id}""#));
    let css = std::fs::read_to_string("static/kernel/jobs.css").unwrap();
    assert!(
        !css.contains("@media (prefers-color-scheme"),
        "a color-scheme media query ignores the switch: use light-dark() tokens"
    );
    assert!(css.contains("--chart-ok: light-dark("));
}

/// At a phone's width no jobs page scrolls sideways: a wide table scrolls inside its `.scroll` box,
/// which contains the visually-hidden labels in its headers (before, /admin/jobs and
/// /admin/jobs/cron were 931 and 634 px wide at 390). `tools/e2e.sh` checks it in a browser.
#[test]
fn wide_tables_scroll_inside_their_box_on_a_phone() {
    let css = std::fs::read_to_string("static/kernel/jobs.css").unwrap();
    assert!(css.contains(".jobs .scroll { position: relative; max-width: 100%; }"));
}

#[tokio::test]
async fn the_dashboard_loads_its_own_css_and_js() {
    let app = TestApp::with_config(development()).await;
    let page = app.get("/admin/jobs").await;
    page.assert_ok();
    for (file, kind) in [
        ("kernel/jobs.css", "text/css"),
        ("kernel/jobs.js", "javascript"),
    ] {
        let link = format!("/static/{file}?v=");
        let at = page
            .body
            .find(&link)
            .unwrap_or_else(|| panic!("{file} is not linked"));
        let url: String = page.body[at..].chars().take_while(|c| *c != '"').collect();
        let res = app.get(&url).await;
        res.assert_ok();
        let content_type = res.headers.get("content-type").unwrap().to_str().unwrap();
        assert!(content_type.contains(kind), "{file}: {content_type}");
    }
    let js = std::fs::read_to_string("static/kernel/jobs.js").unwrap();
    for banned in ["eval(", "new Function", "innerHTML", "document.write"] {
        assert!(!js.contains(banned), "jobs.js uses {banned}");
    }
}

#[test]
fn every_dashboard_class_has_a_css_rule() {
    use offrails_host::kernel::css::has_selector;
    let css = std::fs::read_to_string("static/kernel/admin.css").unwrap()
        + &std::fs::read_to_string("static/kernel/jobs.css").unwrap();
    let hooks = ["row", "row-link", "help", "detail", "cronexpr"];
    let mut missing = Vec::new();
    for entry in std::fs::read_dir("templates/kernel/jobs").unwrap() {
        let path = entry.unwrap().path();
        let src = std::fs::read_to_string(&path).unwrap();
        for attr in src.split("class=\"").skip(1) {
            let mut list = attr.split('"').next().unwrap_or_default().to_owned();
            // Drop Askama tags: `{% if x %}a{% else %}b{% endif %}` keeps a and b.
            while let Some(start) = list.find("{%") {
                let end = list[start..]
                    .find("%}")
                    .map_or(list.len(), |e| start + e + 2);
                list.replace_range(start..end, " ");
            }
            // `st-{{ state }}` is built at render time: mark it and skip it below.
            while let Some(start) = list.find("{{") {
                let end = list[start..]
                    .find("}}")
                    .map_or(list.len(), |e| start + e + 2);
                list.replace_range(start..end, "{}");
            }
            for token in list.split_whitespace() {
                if token.contains('{') || token.contains('}') || token.contains('%') {
                    continue; // built at render time (`st-{{ state }}`)
                }
                if !hooks.contains(&token) && !has_selector(&css, token) {
                    missing.push(format!("{}: {token}", path.display()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "classes with no CSS rule: {missing:#?}");
}

#[tokio::test]
async fn the_overview_and_list_queries_dont_grow_with_jobs() {
    let app = TestApp::with_config(development()).await;
    for path in ["/admin/jobs", "/admin/jobs/list", "/admin/jobs/errors"] {
        app.assert_query_count_stable(path, async |n| {
            for i in 0..n {
                insert(app.db(), builder("fail", json!({"n": i})).max_attempts(1)).await;
            }
            drain(app.db()).await;
        })
        .await;
    }
}
