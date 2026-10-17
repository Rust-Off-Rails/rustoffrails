//! `app jobs …` (the CLI over `kernel::jobs::admin`), with output captured in a buffer.
//! GUIDE: .offrails/guides/jobs-dashboard.md#cli-app-jobs

use clap::Parser;
use offrails_host::{
    jobs::{
        self as app_jobs,
        cli::{JobsCommand, execute},
        examples::DeliverWebhookJob,
    },
    kernel::jobs::{JobBuilder, JobDefaults, JobState, admin},
};
use serde_json::json;

use crate::support::TestApp;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: JobsCommand,
}

async fn jobs(app: &TestApp, args: &[&str]) -> anyhow::Result<String> {
    jobs_answering(app, args, true).await
}

async fn jobs_answering(app: &TestApp, args: &[&str], answer: bool) -> anyhow::Result<String> {
    let cli = Cli::try_parse_from(std::iter::once("jobs").chain(args.iter().copied()))?;
    let mut out = Vec::new();
    execute(&app.state, cli.command, &mut out, |_| answer).await?;
    Ok(String::from_utf8(out).unwrap())
}

/// A value generated per run, so no secret-looking literal lives in the source.
fn fake_secret() -> String {
    format!("FAKE-{}", uuid::Uuid::new_v4().simple())
}

fn secret_job(key: &str, password: &str) -> JobBuilder {
    JobBuilder::new(
        JobDefaults {
            worker: "sync_account",
            queue: "default",
            max_attempts: 1,
            priority: 0,
            unique: None,
        },
        Ok(json!({"account_id": 9, "api_key": key, "nested": {"password": password}})),
    )
}

#[tokio::test]
async fn stats_list_and_show_print_tables_and_json() {
    let app = TestApp::new().await;
    let id = app_jobs::enqueue(
        app.db(),
        DeliverWebhookJob {
            endpoint: "https://down.invalid/hook".into(),
            event: "order.paid".into(),
        },
    )
    .await
    .unwrap()
    .id;
    app.drain_jobs().await;

    let out = jobs(&app, &["stats"]).await.unwrap();
    assert!(out.contains("retryable  1"), "{out}");
    assert!(out.contains("webhooks"), "{out}");
    let v: serde_json::Value =
        serde_json::from_str(&jobs(&app, &["stats", "--json"]).await.unwrap()).unwrap();
    assert!(v["queues"].is_array() && v["stats"]["by_state"].is_array());

    let out = jobs(&app, &["list", "--state", "retryable"]).await.unwrap();
    assert!(
        out.contains(&id.to_string()) && out.contains("deliver_webhook"),
        "{out}"
    );
    assert!(out.contains("retries in"), "{out}");
    let listed: serde_json::Value = serde_json::from_str(
        &jobs(&app, &["list", "--json", "--worker", "deliver_webhook"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(listed[0]["id"], id);
    assert!(jobs(&app, &["list", "--state", "bogus"]).await.is_err());

    let out = jobs(&app, &["show", &id.to_string()]).await.unwrap();
    assert!(out.contains("attempt 1"), "{out}");
    assert!(out.contains("connection refused"), "{out}");
    assert!(
        out.contains(&format!("retry: app jobs retry {id}")),
        "{out}"
    );
    assert!(jobs(&app, &["show", "999999"]).await.is_err());

    let out = jobs(&app, &["errors"]).await.unwrap();
    assert!(
        out.contains("deliver_webhook")
            && out.contains("connection refused (os error N) · down.invalid"),
        "{out}"
    );
}

/// Security: secrets that slipped into args never reach the terminal or JSON output.
#[tokio::test]
async fn sensitive_args_are_redacted_in_the_cli_and_admin_views() {
    let app = TestApp::new().await;
    let (key, password) = (fake_secret(), fake_secret());
    let id = secret_job(&key, &password)
        .insert(app.db())
        .await
        .unwrap()
        .id;
    for args in [
        vec!["list".to_owned()],
        vec!["list".to_owned(), "--json".to_owned()],
        vec!["show".to_owned(), id.to_string()],
        vec!["show".to_owned(), id.to_string(), "--json".to_owned()],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = jobs(&app, &args).await.unwrap();
        assert!(
            !out.contains(&key) && !out.contains(&password),
            "{args:?} leaked: {out}"
        );
        assert!(out.contains("[redacted]"), "{args:?}: {out}");
    }
    // The stored row is untouched: the worker still gets its args.
    let row = admin::get(app.db(), id).await.unwrap().unwrap();
    assert_eq!(row.args["api_key"], key.as_str());
}

#[tokio::test]
async fn bulk_commands_need_a_filter_and_a_confirmation() {
    let app = TestApp::new().await;
    for _ in 0..3 {
        app_jobs::enqueue(
            app.db(),
            DeliverWebhookJob {
                endpoint: "http://plain.example.com".into(),
                event: format!("e{}", rand::random::<u32>()),
            },
        )
        .await
        .unwrap();
    }
    app.drain_jobs().await; // discarded: not https
    assert!(jobs(&app, &["retry", "--all"]).await.is_err(), "no filter");
    assert!(jobs(&app, &["retry"]).await.is_err(), "no ids");

    let out = jobs_answering(&app, &["retry", "--all", "--state", "discarded"], false)
        .await
        .unwrap();
    assert!(out.contains("nothing changed"), "{out}");
    assert_eq!(
        admin::stats(app.db())
            .await
            .unwrap()
            .count(JobState::Discarded),
        3
    );

    let out = jobs(&app, &["retry", "--all", "--state", "discarded", "--yes"])
        .await
        .unwrap();
    assert_eq!(out.trim(), "retried 3 jobs");
    let ids: Vec<String> = admin::list(app.db(), &Default::default(), None, 10)
        .await
        .unwrap()
        .iter()
        .map(|j| j.id.to_string())
        .collect();
    let mut args = vec!["cancel"];
    args.extend(ids.iter().map(String::as_str));
    assert_eq!(jobs(&app, &args).await.unwrap().trim(), "cancelled 3 jobs");
    let mut args = vec!["delete"];
    args.extend(ids.iter().map(String::as_str));
    assert_eq!(jobs(&app, &args).await.unwrap().trim(), "deleted 3 jobs");
}

#[tokio::test]
async fn queues_cron_and_prune_commands() {
    let app = TestApp::new().await;
    assert_eq!(
        jobs(&app, &["pause", "default"]).await.unwrap().trim(),
        "paused default"
    );
    let q = admin::queues(app.db(), chrono::Utc::now()).await.unwrap();
    assert!(q.iter().any(|q| q.name == "default" && q.paused));
    assert!(jobs(&app, &["pause"]).await.is_err());
    assert_eq!(
        jobs(&app, &["resume", "--all"]).await.unwrap().trim(),
        "resumed 1 queues"
    );

    let out = jobs(&app, &["cron"]).await.unwrap();
    assert!(
        out.contains("cleanup") && out.contains("0 * * * *") && out.contains("never"),
        "{out}"
    );
    let out = jobs(&app, &["cron", "--run", "cleanup"]).await.unwrap();
    assert!(out.starts_with("enqueued job"), "{out}");
    assert!(jobs(&app, &["cron", "--run", "nope"]).await.is_err());
    let v: serde_json::Value =
        serde_json::from_str(&jobs(&app, &["cron", "--json"]).await.unwrap()).unwrap();
    let cleanup = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "cleanup")
        .unwrap();
    assert!(cleanup["last_job"].is_i64());
    assert_eq!(app.drain_jobs().await.completed, 1);

    let out = jobs(&app, &["prune", "--days", "1"]).await.unwrap();
    assert!(out.starts_with("pruned 0 jobs"), "{out}");
}
