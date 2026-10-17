//! `app task jobs_demo`: fills the queue with an hour of realistic, synthetic history (every
//! state, failures with error chains, a batch, cron runs, a paused queue and one no node runs),
//! plus live work for the inline worker: reports that report progress and webhooks that fail.
//! Development only. Every name, URL and id is made up (example.com, .invalid).

use chrono::{Duration, Utc};
use rand::RngExt;
use sea_orm::{
    ActiveEnum, ActiveValue::NotSet, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter,
    sea_query::Expr,
};
use serde_json::{Value, json};

use crate::{
    jobs::{
        self as app_jobs, Batch, Worker,
        examples::{DeliverWebhookJob, ExportReportJob, ReportBatchDoneJob},
    },
    kernel::jobs::{JobState, admin, insert_all, job, recurring_execution},
    state::AppState,
};

/// A finished or waiting job row written directly (history the demo didn't really run).
struct Row {
    worker: &'static str,
    queue: &'static str,
    args: Value,
    state: JobState,
    attempt: i32,
    max_attempts: i32,
    tags: Vec<&'static str>,
    errors: Vec<(&'static str, Option<&'static str>)>,
    minutes_ago: i64,
    wait_ms: i64,
    run_ms: i64,
}

const DETAIL_HTTP: &str = "POST https://hooks.example.com/orders (order.paid)\n\nCaused by:\n    0: connecting to hooks.example.com:443\n    1: connection refused (os error 61)";

pub async fn run(state: &AppState, _args: Vec<String>) -> anyhow::Result<()> {
    anyhow::ensure!(
        state.config.is_development(),
        "jobs_demo writes synthetic jobs: development only"
    );
    let db = &state.db;
    let now = Utc::now();
    let mut rng = rand::rng();
    let mut rows: Vec<Row> = Vec::new();

    // An hour of completed work, busier lately, with a failure burst 20 minutes ago.
    let workers: [(&str, &str, i64); 6] = [
        ("send_receipt_email", "mailers", 900),
        ("deliver_webhook", "webhooks", 350),
        ("export_report", "default", 6_500),
        ("sync_inventory", "default", 1_800),
        ("resize_image", "default", 420),
        ("cleanup", "default", 120),
    ];
    for minute in 0..60_i64 {
        let busy = 3 + (60 - minute) / 6 + rng.random_range(0..5);
        for _ in 0..busy {
            let (worker, queue, base) = workers[rng.random_range(0..workers.len())];
            let failed = (18..=23).contains(&minute) && worker == "deliver_webhook"
                || rng.random_range(0..100) < 2;
            rows.push(Row {
                worker,
                queue,
                args: demo_args(worker, &mut rng),
                state: if failed {
                    JobState::Discarded
                } else {
                    JobState::Completed
                },
                attempt: if failed { 5 } else { 1 },
                max_attempts: 5,
                tags: if worker == "send_receipt_email" {
                    vec!["billing"]
                } else {
                    vec![]
                },
                errors: if failed { failure(worker) } else { vec![] },
                minutes_ago: minute,
                wait_ms: rng.random_range(5..400) + if minute < 8 { 2_500 } else { 0 },
                run_ms: base / 2 + rng.random_range(0..base.max(2)),
            });
        }
    }
    // Retryable: failing now, retrying with backoff.
    for (i, (msg, detail)) in [
        (
            "charging subscription 4812: card_declined: insufficient funds",
            None,
        ),
        (
            "syncing store 31: inventory API returned HTTP 429 Too Many Requests",
            None,
        ),
        (
            "syncing store 77: inventory API returned HTTP 429 Too Many Requests",
            None,
        ),
        (
            "resizing upload 9021: unsupported image format (image/heic)",
            Some("resizing upload 9021\n\nCaused by:\n    unsupported image format (image/heic)"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let worker = match i {
            0 => "charge_subscription",
            3 => "resize_image",
            _ => "sync_inventory",
        };
        rows.push(Row {
            worker,
            queue: "default",
            args: demo_args(worker, &mut rng),
            state: JobState::Retryable,
            attempt: 2 + i as i32,
            max_attempts: 10,
            tags: vec!["retrying"],
            errors: (0..2 + i).map(|_| (msg, detail)).collect(),
            minutes_ago: 3 + i as i64,
            wait_ms: 40,
            run_ms: 800,
        });
    }
    // Cancelled: the work stopped mattering.
    rows.push(Row {
        worker: "send_receipt_email",
        queue: "mailers",
        args: json!({"order_id": 55102}),
        state: JobState::Cancelled,
        attempt: 1,
        max_attempts: 5,
        tags: vec!["billing"],
        errors: vec![(
            "cancelled: order 55102 was refunded before the receipt went out",
            None,
        )],
        minutes_ago: 41,
        wait_ms: 30,
        run_ms: 12,
    });
    // A queue no node runs (its jobs wait), and scheduled work.
    for n in 0..3 {
        rows.push(Row {
            worker: "export_report",
            queue: "reports-eu",
            args: json!({"report_id": 300 + n, "rows": 25_000}),
            state: JobState::Available,
            attempt: 0,
            max_attempts: 5,
            tags: vec!["region:eu"],
            errors: vec![],
            minutes_ago: 0,
            wait_ms: 0,
            run_ms: 0,
        });
    }

    let mut models = Vec::with_capacity(rows.len());
    for r in rows {
        let finished =
            now - Duration::minutes(r.minutes_ago) - Duration::seconds(rng.random_range(0..59));
        let attempted = finished - Duration::milliseconds(r.run_ms);
        let scheduled = attempted - Duration::milliseconds(r.wait_ms);
        let errors: Vec<Value> = r
            .errors
            .iter()
            .enumerate()
            .map(|(i, (msg, detail))| {
                let mut e = json!({
                    "attempt": i + 1,
                    "at": (attempted - Duration::minutes((r.errors.len() - i) as i64 * 3)).to_rfc3339(),
                    "error": msg,
                    "node": "node-4b1e09a2",
                    "duration_ms": 30_000 / (i as i64 + 1),
                });
                if let Some(d) = detail {
                    e["detail"] = json!(d);
                }
                e
            })
            .collect();
        let pending = matches!(r.state, JobState::Available | JobState::Scheduled);
        models.push(job::ActiveModel {
            id: NotSet,
            state: Set(r.state),
            queue: Set(r.queue.to_owned()),
            worker: Set(r.worker.to_owned()),
            args: Set(r.args),
            meta: Set(json!({})),
            tags: Set(json!(r.tags)),
            errors: Set(Value::Array(errors)),
            attempt: Set(r.attempt),
            max_attempts: Set(r.max_attempts),
            priority: Set(if r.worker == "export_report" { 3 } else { 0 }),
            progress: Set((r.state == JobState::Completed).then_some(100)),
            batch_id: Set(None),
            unique_key: Set(None),
            attempted_by: Set((!pending).then(|| "node-4b1e09a2".to_owned())),
            inserted_at: Set(scheduled
                - Duration::minutes(r.errors.len() as i64 * 3)
                - Duration::seconds(1)),
            scheduled_at: Set(if r.state == JobState::Retryable {
                now + Duration::minutes(rng.random_range(2..40))
            } else if pending {
                now - Duration::minutes(4)
            } else {
                scheduled
            }),
            attempted_at: Set((!pending).then_some(attempted)),
            completed_at: Set((r.state == JobState::Completed).then_some(finished)),
            cancelled_at: Set((r.state == JobState::Cancelled).then_some(finished)),
            discarded_at: Set((r.state == JobState::Discarded).then_some(finished)),
        });
    }
    let history = models.len();
    for chunk in models.chunks(400) {
        job::Entity::insert_many(chunk.to_vec())
            .exec_without_returning(db)
            .await?;
    }

    // Live work for the inline worker: long reports with progress, scheduled jobs, failing hooks.
    for (i, rows) in [300_000_u32, 700_000, 1_500_000].into_iter().enumerate() {
        app_jobs::enqueue(
            db,
            ExportReportJob {
                report_id: 1_000 + i as i64,
                rows,
            },
        )
        .await?;
    }
    for (i, minutes) in [7_i64, 25, 90, 240].into_iter().enumerate() {
        ExportReportJob {
            report_id: 2_000 + i as i64,
            rows: 20_000,
        }
        .into_job()
        .schedule_in(Duration::minutes(minutes))
        .tags(["nightly", "region:us"])
        .insert(db)
        .await?;
    }
    for event in ["order.paid", "order.refunded", "customer.updated"] {
        app_jobs::enqueue(
            db,
            DeliverWebhookJob {
                endpoint: "https://hooks.partner.invalid/v2/events".into(),
                event: event.into(),
            },
        )
        .await?;
    }
    app_jobs::enqueue(
        db,
        DeliverWebhookJob {
            endpoint: "https://busy.example.com/inbound".into(),
            event: "order.paid".into(),
        },
    )
    .await?;

    // A batch: twelve report chunks and a callback when they're all done.
    let batch = Batch::new("Quarterly report 2026-Q3")
        .on_complete(ReportBatchDoneJob { report_id: 42 }.into_job())
        .insert(
            db,
            (0..12)
                .map(|i| {
                    ExportReportJob {
                        report_id: 4_200 + i,
                        rows: 40_000,
                    }
                    .into_job()
                    .tag("batch:q3")
                })
                .collect(),
        )
        .await?;

    // Cron history: each entry ran at its last due time.
    for entry in app_jobs::recurring::SCHEDULE {
        let Some(run_at) = crate::kernel::jobs::previous_run(entry.cron, now) else {
            continue;
        };
        let inserted = (entry.job)()
            .tag("cron")
            .meta(json!({"cron": entry.key}))
            .insert(db)
            .await?;
        job::Entity::update_many()
            .col_expr(
                job::Column::State,
                Expr::value(JobState::Completed.to_value()),
            )
            .col_expr(
                job::Column::AttemptedAt,
                Expr::value(run_at + Duration::milliseconds(80)),
            )
            .col_expr(
                job::Column::CompletedAt,
                Expr::value(run_at + Duration::milliseconds(900)),
            )
            .col_expr(job::Column::Attempt, Expr::value(1))
            .col_expr(job::Column::UniqueKey, Expr::value(Option::<String>::None))
            .filter(job::Column::Id.eq(inserted.id))
            .exec(db)
            .await?;
        recurring_execution::Entity::insert(recurring_execution::ActiveModel {
            id: NotSet,
            key: Set(entry.key.to_owned()),
            run_at: Set(run_at),
            job_id: Set(Some(inserted.id)),
        })
        .on_conflict_do_nothing_on([
            recurring_execution::Column::Key,
            recurring_execution::Column::RunAt,
        ])
        .exec(db)
        .await?;
    }

    // Fan-out with insert_all, and a paused queue.
    insert_all(
        db,
        (0..6)
            .map(|i| {
                DeliverWebhookJob {
                    endpoint: format!("https://hooks.example.com/tenants/{}", 10 + i),
                    event: "invoice.created".into(),
                }
                .into_job()
                .without_unique()
                .queue("mailers")
            })
            .collect(),
    )
    .await?;
    admin::pause(db, "mailers").await?;

    println!(
        "jobs demo: {history} history rows, live reports and webhooks, batch {}, cron runs; queue mailers paused",
        batch.id
    );
    Ok(())
}

/// Five failed attempts that read like the worker's real failures.
fn failure(worker: &str) -> Vec<(&'static str, Option<&'static str>)> {
    let (a, b): (&'static str, &'static str) = match worker {
        "deliver_webhook" => (
            "POST https://hooks.example.com/orders (order.paid): connecting to hooks.example.com:443: connection refused (os error 61)",
            "POST https://hooks.example.com/orders (order.paid): HTTP 503 Service Unavailable",
        ),
        "send_receipt_email" => (
            "sending receipt for order 55021: SMTP 451 4.7.1 try again later",
            "sending receipt for order 55021: SMTP 451 4.7.1 try again later",
        ),
        "sync_inventory" => (
            "syncing store 12: inventory API returned HTTP 502 Bad Gateway",
            "syncing store 12: reading the response: unexpected end of JSON input at line 1 column 8192",
        ),
        "resize_image" => (
            "resizing upload 8812: decoding image: corrupt JPEG data: premature end of data segment",
            "resizing upload 8812: decoding image: corrupt JPEG data: premature end of data segment",
        ),
        "export_report" => ("timed out after 600s", "timed out after 600s"),
        _ => (
            "deleting expired cache entries: database is locked (code 5)",
            "deleting expired cache entries: database is locked (code 5)",
        ),
    };
    let detail = (worker == "deliver_webhook").then_some(DETAIL_HTTP);
    vec![(a, detail), (a, detail), (b, None), (b, None), (b, None)]
}

fn demo_args(worker: &str, rng: &mut impl rand::Rng) -> Value {
    match worker {
        "send_receipt_email" => json!({"order_id": rng.random_range(50_000..56_000)}),
        "deliver_webhook" => {
            json!({"endpoint": "https://hooks.example.com/orders", "event": "order.paid"})
        }
        "export_report" => json!({"report_id": rng.random_range(100..900), "rows": 5_000}),
        "sync_inventory" => json!({"store_id": rng.random_range(1..90), "full": false}),
        "resize_image" => {
            json!({"upload_id": rng.random_range(8_000..9_500), "sizes": [320, 640, 1280]})
        }
        "charge_subscription" => json!({"subscription_id": rng.random_range(4_000..5_000)}),
        _ => json!(null),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_urls_are_synthetic() {
        let source = include_str!("jobs_demo.rs");
        for host in source.split("https://").skip(1) {
            let host = host.split('/').next().unwrap_or_default();
            if host.contains('"') {
                continue; // this test's own code
            }
            assert!(
                host.ends_with("example.com") || host.ends_with(".invalid") || host.is_empty(),
                "{host}"
            );
        }
    }
}
