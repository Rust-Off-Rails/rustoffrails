//! The long-running worker (`kernel::jobs::run`): producers, heartbeat, cancellation, pause,
//! progress, graceful shutdown, rescue at boot, cron, and the inline worker.
//! GUIDE: .offrails/guides/jobs.md#worker-runtime-executorrs

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use offrails_host::{
    config::Config,
    jobs::{self as app_jobs, examples::ExportReportJob},
    kernel::jobs::{
        DispatchFn, JobBuilder, JobDefaults, JobState, Outcome, QueueSpec, Recurring, WorkerConfig,
        WorkerOptions, admin, executor, job, job_node, run,
    },
    state::AppState,
};
use sea_orm::{ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::json;
use tokio::sync::{Notify, oneshot};

use crate::support::{TestApp, wait_until};

#[derive(Default)]
struct Probe {
    ran: Mutex<Vec<i64>>,
    dropped: AtomicBool,
    release: Notify,
}

/// Sets `dropped` when the future holding it is dropped (an abort).
struct DropFlag(Arc<Probe>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

fn dispatcher(probe: Arc<Probe>) -> DispatchFn {
    Arc::new(move |worker, _args, job| {
        let probe = probe.clone();
        Box::pin(async move {
            match worker.as_str() {
                "count" => {
                    probe.ran.lock().unwrap().push(job.id);
                    Ok(Outcome::Done)
                }
                "hang" => {
                    let _flag = DropFlag(probe.clone());
                    std::future::pending::<()>().await;
                    Ok(Outcome::Done)
                }
                "progress" => {
                    job.progress(40).await;
                    probe.release.notified().await;
                    Ok(Outcome::Done)
                }
                other => Ok(Outcome::Discard(format!("unknown worker: {other}"))),
            }
        })
    })
}

fn config(_worker: &str) -> WorkerConfig {
    WorkerConfig {
        timeout: Duration::from_secs(30),
        backoff: |_| chrono::Duration::seconds(60),
    }
}

fn builder(worker: &'static str) -> JobBuilder {
    JobBuilder::new(
        JobDefaults {
            worker,
            queue: "default",
            max_attempts: 3,
            priority: 0,
            unique: None,
        },
        Ok(json!({})),
    )
}

const NO_CRON: &[Recurring] = &[];

/// A running worker and the switch that stops it.
struct Running {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    async fn shutdown(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}

fn start(
    db: &DatabaseConnection,
    node: &str,
    probe: Arc<Probe>,
    grace: Duration,
    schedule: &'static [Recurring],
) -> Running {
    let (stop, stopped) = oneshot::channel::<()>();
    let opts = WorkerOptions {
        queues: vec![QueueSpec::new("default", 4)],
        node: node.to_owned(),
        prune_after: chrono::Duration::days(7),
        shutdown_grace: grace,
        schedule,
    };
    let db = db.clone();
    let task = tokio::spawn(async move {
        run(db, opts, dispatcher(probe), config, async move {
            let _ = stopped.await;
        })
        .await;
    });
    Running { stop, task }
}

async fn state_of(db: &DatabaseConnection, id: i64) -> JobState {
    admin::get(db, id).await.unwrap().unwrap().state
}

async fn node_exists(db: &DatabaseConnection, node: &str) -> bool {
    job_node::Entity::find_by_id(node.to_owned())
        .one(db)
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn a_worker_runs_jobs_and_deregisters_on_shutdown() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    wait_until("the node registers", async || {
        node_exists(app.db(), "node-a").await
    })
    .await;
    let mut ids = Vec::new();
    for _ in 0..10 {
        ids.push(builder("count").insert(app.db()).await.unwrap().id);
    }
    wait_until("every job completes", async || {
        probe.ran.lock().unwrap().len() == 10
    })
    .await;
    for id in &ids {
        wait_until("recorded", async || {
            state_of(app.db(), *id).await == JobState::Completed
        })
        .await;
    }
    let node = job_node::Entity::find_by_id("node-a".to_owned())
        .one(app.db())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.queues["default"]["limit"], 4);
    worker.shutdown().await;
    assert!(!node_exists(app.db(), "node-a").await, "deregistered");
}

/// INVARIANT: a job is claimed by one worker only (two whole runtimes on one database).
#[tokio::test]
async fn two_workers_never_run_the_same_job() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    offrails_host::kernel::jobs::insert_all(app.db(), (0..40).map(|_| builder("count")).collect())
        .await
        .unwrap();
    let a = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    let b = start(
        app.db(),
        "node-b",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    wait_until("all 40 ran", async || probe.ran.lock().unwrap().len() >= 40).await;
    wait_until("all recorded", async || {
        admin::stats(app.db())
            .await
            .unwrap()
            .count(JobState::Completed)
            == 40
    })
    .await;
    a.shutdown().await;
    b.shutdown().await;
    let mut ran = probe.ran.lock().unwrap().clone();
    assert_eq!(ran.len(), 40);
    ran.sort_unstable();
    ran.dedup();
    assert_eq!(ran.len(), 40, "a job ran twice");
}

/// INVARIANT: cancelling an executing job stops its future.
#[tokio::test]
async fn cancelling_an_executing_job_aborts_it() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    let id = builder("hang").insert(app.db()).await.unwrap().id;
    wait_until("it runs", async || {
        state_of(app.db(), id).await == JobState::Executing
    })
    .await;
    assert_eq!(admin::cancel(app.db(), &[id]).await.unwrap(), 1);
    wait_until("the heartbeat aborts it", async || {
        probe.dropped.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(state_of(app.db(), id).await, JobState::Cancelled);
    worker.shutdown().await;
}

#[tokio::test]
async fn a_paused_queue_is_not_fetched_until_resumed() {
    let app = TestApp::new().await;
    admin::pause(app.db(), "default").await.unwrap();
    let id = builder("count").insert(app.db()).await.unwrap().id;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    wait_until("the node registers", async || {
        node_exists(app.db(), "node-a").await
    })
    .await;
    // Two more heartbeats: the producer has had time to fetch, and didn't.
    let first = job_node::Entity::find_by_id("node-a".to_owned())
        .one(app.db())
        .await
        .unwrap()
        .unwrap()
        .heartbeat_at;
    wait_until("two heartbeats pass", async || {
        let now = job_node::Entity::find_by_id("node-a".to_owned())
            .one(app.db())
            .await
            .unwrap()
            .unwrap()
            .heartbeat_at;
        now - first >= chrono::Duration::milliseconds(1500)
    })
    .await;
    assert_eq!(state_of(app.db(), id).await, JobState::Available);
    assert!(probe.ran.lock().unwrap().is_empty());

    admin::resume(app.db(), "default").await.unwrap();
    wait_until("it runs once resumed", async || {
        state_of(app.db(), id).await == JobState::Completed
    })
    .await;
    worker.shutdown().await;
}

/// INVARIANT: graceful shutdown never loses a job: unfinished ones go back to `available`.
#[tokio::test]
async fn shutdown_returns_unfinished_jobs_to_the_queue() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_millis(100),
        NO_CRON,
    );
    let id = builder("hang")
        .max_attempts(1)
        .insert(app.db())
        .await
        .unwrap()
        .id;
    wait_until("it runs", async || {
        state_of(app.db(), id).await == JobState::Executing
    })
    .await;
    worker.shutdown().await;
    assert!(
        probe.dropped.load(Ordering::SeqCst),
        "aborted after the grace period"
    );
    let j = admin::get(app.db(), id).await.unwrap().unwrap();
    assert_eq!(j.state, JobState::Available, "handed back at once");
    assert_eq!(
        j.max_attempts, 2,
        "a deploy doesn't use up the last attempt"
    );
    let last = j.errors.as_array().unwrap().last().unwrap().clone();
    assert!(
        last["error"]
            .as_str()
            .unwrap()
            .starts_with("interrupted: node node-a shut down"),
        "{last}"
    );
    assert!(!node_exists(app.db(), "node-a").await);
}

#[tokio::test]
async fn progress_shows_while_a_job_runs() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    let id = builder("progress").insert(app.db()).await.unwrap().id;
    wait_until("progress is visible mid-run", async || {
        let j = admin::get(app.db(), id).await.unwrap().unwrap();
        j.state == JobState::Executing && j.progress == Some(40)
    })
    .await;
    let running = admin::running(app.db(), 10).await.unwrap();
    assert_eq!(running.iter().map(|j| j.id).collect::<Vec<_>>(), [id]);
    probe.release.notify_one();
    wait_until("it completes", async || {
        state_of(app.db(), id).await == JobState::Completed
    })
    .await;
    assert_eq!(
        admin::get(app.db(), id).await.unwrap().unwrap().progress,
        Some(100)
    );
    worker.shutdown().await;
}

/// Crash recovery: a node died mid-job (its heartbeat went stale). A worker that boots later
/// rescues the job and runs it.
#[tokio::test]
async fn a_dead_nodes_job_is_rescued_and_run_by_the_next_worker() {
    let app = TestApp::new().await;
    let id = builder("count").insert(app.db()).await.unwrap().id;
    executor::claim(app.db(), None, 1, "node-crashed", Utc::now())
        .await
        .unwrap();
    job_node::Entity::insert(job_node::ActiveModel {
        node: Set("node-crashed".into()),
        queues: Set(json!({"default": {"limit": 4}})),
        started_at: Set(Utc::now() - chrono::Duration::hours(1)),
        heartbeat_at: Set(Utc::now() - chrono::Duration::minutes(3)),
    })
    .exec(app.db())
    .await
    .unwrap();
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-b",
        probe.clone(),
        Duration::from_secs(1),
        NO_CRON,
    );
    wait_until("rescued and run", async || {
        state_of(app.db(), id).await == JobState::Completed
    })
    .await;
    let j = admin::get(app.db(), id).await.unwrap().unwrap();
    assert_eq!(j.attempt, 2);
    assert_eq!(j.attempted_by.as_deref(), Some("node-b"));
    assert!(
        j.errors.as_array().unwrap()[0]["error"]
            .as_str()
            .unwrap()
            .starts_with("orphaned: node node-crashed")
    );
    worker.shutdown().await;
}

const EVERY_MINUTE: &[Recurring] = &[Recurring {
    key: "tick",
    cron: "* * * * *",
    job: || builder("count"),
}];

#[tokio::test]
async fn the_worker_fires_cron_entries() {
    let app = TestApp::new().await;
    let probe = Arc::new(Probe::default());
    let worker = start(
        app.db(),
        "node-a",
        probe.clone(),
        Duration::from_secs(1),
        EVERY_MINUTE,
    );
    wait_until("the cron job ran", async || {
        !probe.ran.lock().unwrap().is_empty()
    })
    .await;
    worker.shutdown().await;
    let cron_jobs = job::Entity::find()
        .filter(job::Column::Worker.eq("count"))
        .all(app.db())
        .await
        .unwrap();
    assert_eq!(cron_jobs.len(), 1, "once per due time");
    assert_eq!(cron_jobs[0].meta["cron"], "tick");
}

#[tokio::test]
async fn the_inline_worker_runs_jobs_inside_serve() {
    let app = TestApp::new().await;
    let mut config = Config::test();
    config.jobs.inline_worker = true;
    config.jobs.node = Some("web-1".into());
    let state = AppState::new(config, app.db().clone()).unwrap();
    let inline = app_jobs::start_inline_worker(&state).expect("configured");
    let id = app_jobs::enqueue(
        app.db(),
        ExportReportJob {
            report_id: 7,
            rows: 10,
        },
    )
    .await
    .unwrap()
    .id;
    wait_until("the inline worker runs it", async || {
        state_of(app.db(), id).await == JobState::Completed
    })
    .await;
    assert_eq!(
        admin::get(app.db(), id)
            .await
            .unwrap()
            .unwrap()
            .attempted_by
            .as_deref(),
        Some("web-1"),
        "APP_JOBS_NODE names the node"
    );
    inline.stop().await;
    assert!(!node_exists(app.db(), "web-1").await);

    // Off unless configured.
    assert!(app_jobs::start_inline_worker(&app.state).is_none());
}
