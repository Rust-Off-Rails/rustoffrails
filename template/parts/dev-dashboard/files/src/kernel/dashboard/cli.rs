//! The dashboard from a terminal:
//!
//! - `app bench --rps 50 --secs 10 --route /dev/lab/orders [--url http://localhost:3000]`: the load
//!   lab's generator against a running server, with a latency summary;
//! - `app test [--sqlite]`: nextest on SQLite, then on Postgres when `TEST_DATABASE_URL` is set,
//!   saved for the Tests page (`storage/test-runs/`);
//! - `app dashboard demo`: fake orders and a history of synthetic test runs (the demo, screenshots);
//! - `app dashboard warmup --url …`: a minute of mixed traffic and every request scenario, so the
//!   charts have something to show.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use clap::{Args, Subcommand};
use sea_orm::DatabaseConnection;

use super::lab::{self, Plan, Run, Target};
use super::testruns::{self, Db};
use super::web::micros;
use crate::config::Config;

#[derive(Args, Debug)]
pub struct BenchArgs {
    /// Requests per second (at most 200).
    #[arg(long, default_value_t = 20)]
    pub rps: u32,
    /// Seconds (at most 60).
    #[arg(long, default_value_t = 10)]
    pub secs: u32,
    /// A GET route of this app; repeat for several (round-robin).
    #[arg(long = "route", required = true)]
    pub routes: Vec<String>,
    /// The running server (default: APP_URL).
    #[arg(long)]
    pub url: Option<String>,
}

#[derive(Args, Debug)]
pub struct TestArgs {
    /// SQLite only, even when TEST_DATABASE_URL is set.
    #[arg(long)]
    pub sqlite: bool,
}

#[derive(Subcommand, Debug)]
pub enum DashboardCommand {
    /// Fake orders and a history of synthetic test runs.
    Demo,
    /// Mixed traffic plus every request scenario against a running server.
    Warmup {
        #[arg(long)]
        url: Option<String>,
        /// Seconds of background traffic.
        #[arg(long, default_value_t = 20)]
        secs: u32,
    },
}

fn base(config: &Config, url: Option<String>) -> String {
    url.unwrap_or_else(|| config.app_url.clone())
        .trim_end_matches('/')
        .to_owned()
}

/// Runs a plan against `url` and prints the result.
pub async fn bench_plan(url: &str, label: &str, plan: Plan) -> Arc<Run> {
    let target = Target::Address(url.to_owned());
    let run = Arc::new(Run::new(label, plan, &target));
    lab::run(target, run.clone()).await;
    run
}

fn summary(run: &Run) -> String {
    let h = super::lock(&run.latency).clone();
    format!(
        "{}: {} sent, {} ok, {} failed, {} skipped (at the in-flight cap) · p50 {} · p95 {} · p99 {} · max {}",
        run.label,
        run.sent.load(Ordering::Relaxed),
        run.ok.load(Ordering::Relaxed),
        run.failed.load(Ordering::Relaxed),
        run.skipped.load(Ordering::Relaxed),
        micros(h.quantile(0.5)),
        micros(h.quantile(0.95)),
        micros(h.quantile(0.99)),
        micros(h.max()),
    )
}

/// `app bench`.
pub async fn bench(config: &Config, args: BenchArgs) -> anyhow::Result<()> {
    let plan = lab::plan(&args.routes, args.rps, args.secs).map_err(anyhow::Error::msg)?;
    let url = base(config, args.url);
    println!(
        "bench: {} req/s for {} s against {url} ({})",
        plan.rps,
        plan.secs,
        plan.paths.join(", ")
    );
    let run = bench_plan(&url, "bench", plan).await;
    println!("{}", summary(&run));
    if run.ok.load(Ordering::Relaxed) == 0 {
        anyhow::bail!("no request succeeded: is the server running at {url}?");
    }
    Ok(())
}

/// `app test`.
pub async fn test(config: &Config, args: TestArgs) -> anyhow::Result<()> {
    let mut dbs = vec![Db::Sqlite];
    let pg = config.dashboard.test_database_url.clone();
    if pg.is_some() && !args.sqlite {
        dbs.push(Db::Postgres);
    }
    let target = config
        .dashboard
        .cargo_target_dir
        .clone()
        .unwrap_or_else(|| "target".into());
    let run = testruns::run(
        Path::new("."),
        Path::new(&target),
        &dbs,
        pg.as_deref(),
        "app test",
        |line| eprintln!("{line}"),
    )
    .await
    .map_err(anyhow::Error::msg)?;
    for s in &run.suites {
        println!(
            "{}: {} passed, {} failed, {} skipped, {} flaky in {}",
            s.db.map_or("?", Db::label),
            s.passed,
            s.failed,
            s.skipped,
            s.flaky,
            crate::kernel::ui::duration(s.duration_ms as i64)
        );
    }
    println!(
        "saved to {}/{} (the dashboard's Tests page)",
        testruns::DIR,
        run.id
    );
    if run.failed() > 0 {
        anyhow::bail!("{} test(s) failed", run.failed());
    }
    Ok(())
}

/// `app dashboard …`.
pub async fn run(
    config: &Config,
    db: &DatabaseConnection,
    command: DashboardCommand,
) -> anyhow::Result<()> {
    match command {
        DashboardCommand::Demo => {
            if lab::count(db).await? < 100 {
                lab::seed(db, 200).await?;
            }
            let n = demo_test_runs(Path::new(testruns::DIR))?;
            println!(
                "dashboard demo: fake orders, and {n} synthetic test runs in {}",
                testruns::DIR
            );
            Ok(())
        }
        DashboardCommand::Warmup { url, secs } => {
            let url = base(config, url);
            let mixed = lab::plan(
                &[
                    "/",
                    "/up",
                    "/dev/lab/orders",
                    "/dev/lab/orders",
                    "/dev/lab/orders/n-plus-one",
                    "/dev/routes",
                ]
                .iter()
                .filter(|p| lab::allowed_path(p))
                .map(|p| (*p).to_owned())
                .collect::<Vec<_>>(),
                25,
                secs.clamp(1, lab::MAX_SECS),
            )
            .map_err(anyhow::Error::msg)?;
            let background = tokio::spawn({
                let url = url.clone();
                async move { bench_plan(&url, "mixed traffic", mixed).await }
            });
            for s in lab::Scenario::ALL {
                if let Some(plan) = s.plan() {
                    let run = bench_plan(&url, s.label(), plan).await;
                    println!("{}", summary(&run));
                }
            }
            // A link to nowhere, as a browser following a typo would send it.
            let client = reqwest::Client::new();
            for _ in 0..3 {
                let _ = client
                    .get(format!("{url}/dev/lab/ordrs"))
                    .header("referer", format!("{url}/dev/lab/orders"))
                    .send()
                    .await;
            }
            if let Ok(run) = background.await {
                println!("{}", summary(&run));
            }
            Ok(())
        }
    }
}

/// Writes a believable history of test runs (fixture data, clearly synthetic test names).
pub fn demo_test_runs(dir: &Path) -> anyhow::Result<usize> {
    const TESTS: &[(&str, &str, u64)] = &[
        ("orders_test", "an_order_total_includes_every_item", 41),
        (
            "orders_test",
            "a_refunded_order_is_not_counted_in_revenue",
            37,
        ),
        ("orders_test", "the_orders_page_query_count_is_stable", 118),
        ("checkout_test", "checkout_rejects_an_empty_cart", 22),
        ("checkout_test", "checkout_sends_a_receipt_email", 260),
        (
            "checkout_test",
            "a_declined_card_shows_the_error_inline",
            74,
        ),
        ("accounts_test", "sign_up_sends_a_verification_code", 310),
        ("accounts_test", "a_reset_link_works_once", 290),
        ("accounts_test", "login_is_rate_limited", 820),
        (
            "security_test",
            "non_public_routes_never_answer_2xx_anonymously",
            1_450,
        ),
        (
            "security_test",
            "cross_site_posts_to_every_unsafe_route_are_403",
            980,
        ),
        (
            "headers_test",
            "every_response_class_carries_the_header_matrix",
            160,
        ),
        ("jobs_test", "a_failed_job_is_retried_with_backoff", 540),
        ("jobs_test", "the_receipt_job_is_unique_per_order", 88),
        (
            "reports_test",
            "the_monthly_report_matches_the_ledger",
            2_300,
        ),
        ("search_test", "search_finds_orders_by_reference", 66),
    ];
    std::fs::create_dir_all(dir)?;
    let now = chrono::Utc::now();
    let runs = 14;
    for i in 0..runs {
        let at = now - chrono::Duration::hours(i64::from(runs - i) * 5);
        let id = at.format("%Y%m%d-%H%M%S").to_string();
        let folder = dir.join(&id);
        std::fs::create_dir_all(&folder)?;
        let mut suites = Vec::new();
        for db in [Db::Sqlite, Db::Postgres] {
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"nextest-run\">\n<testsuite name=\"app\">\n",
            );
            let mut s = testruns::SuiteSummary {
                db: Some(db),
                ..Default::default()
            };
            let mut total_ms = 0;
            for (k, (module, name, ms)) in TESTS.iter().enumerate() {
                let slower = if db == Db::Postgres { 3 } else { 1 };
                let ms = ms * slower + (i as u64 * 7 + k as u64 * 13) % 40;
                total_ms += ms;
                let classname = format!("app::{module}");
                // One test flips on Postgres now and then; the newest Postgres run has a failure.
                let flaky = *name == "login_is_rate_limited" && db == Db::Postgres && i % 4 == 1;
                let failed = (*name == "the_monthly_report_matches_the_ledger"
                    && db == Db::Postgres
                    && i == runs - 1)
                    || (*name == "login_is_rate_limited" && db == Db::Postgres && i == 6);
                xml.push_str(&format!(
                    "<testcase name=\"{name}\" classname=\"{classname}\" time=\"{:.3}\">",
                    ms as f64 / 1000.0
                ));
                if failed {
                    s.failed += 1;
                    xml.push_str("<failure message=\"assertion `left == right` failed\" type=\"test failure\">thread 'reports_test::the_monthly_report_matches_the_ledger' panicked at tests/app/reports_test.rs:48:5:\nassertion `left == right` failed: totals differ by one cent\n  left: 1204599\n right: 1204600\nnote: rounding happens per line on Postgres (NUMERIC) and per order on SQLite</failure>");
                } else if flaky {
                    s.passed += 1;
                    s.flaky += 1;
                    xml.push_str("<flakyFailure message=\"timed out waiting for: the lockout\" type=\"test failure\">timed out waiting for: the lockout</flakyFailure>");
                } else {
                    s.passed += 1;
                }
                xml.push_str("</testcase>\n");
            }
            xml.push_str("</testsuite>\n</testsuites>\n");
            std::fs::write(folder.join(format!("{}.xml", db.key())), xml)?;
            s.duration_ms = total_ms / 3
                + 4_000
                + (i as u64 * 37 % 9) * 450
                + if db == Db::Postgres {
                    9_000 + (i as u64 * 53 % 11) * 900
                } else {
                    0
                };
            suites.push(s);
        }
        testruns::write_summary(
            dir,
            &testruns::RunSummary {
                id,
                at,
                source: if i % 3 == 0 {
                    "dashboard".into()
                } else {
                    "app test".into()
                },
                suites,
            },
        )
        .map_err(anyhow::Error::msg)?;
    }
    std::fs::write(
        dir.join("coverage.json"),
        r#"{"type":"llvm.coverage.json.export","data":[{"totals":{"lines":{"count":5210,"covered":4421,"percent":84.85604606525911}}}]}"#,
    )?;
    Ok(runs as usize)
}
