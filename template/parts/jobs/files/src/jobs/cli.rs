//! `app jobs …`: the same operations as `/admin/jobs`, for terminals, scripts and agents. Output
//! is an aligned table, or JSON with `--json`. Everything goes through `kernel::jobs::admin`, so
//! the browser and the terminal can't disagree.
//! GUIDE: .offrails/guides/jobs-dashboard.md#cli-app-jobs
//!
//! Agents: when "X never happened", run `app jobs list --state discarded --json` and
//! `app jobs show <id>` before reading code.

use std::io::{BufRead, Write};

use chrono::Utc;
use clap::{Args, Subcommand};
use sea_orm::DatabaseConnection;
use serde_json::json;

use crate::{
    kernel::{
        jobs::{
            JobState,
            admin::{self, Action, JobFilter},
            job, next_run, plugins, prune, redact,
        },
        ui::{ago, duration, until},
    },
    state::AppState,
};

#[derive(Debug, Args, Default, Clone)]
pub struct FilterArgs {
    /// available | scheduled | executing | retryable | completed | discarded | cancelled
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long)]
    pub queue: Option<String>,
    #[arg(long)]
    pub worker: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// An id, or text to find in args or the worker name.
    #[arg(long)]
    pub q: Option<String>,
    /// Text to find in the recorded errors.
    #[arg(long)]
    pub error: Option<String>,
}

impl FilterArgs {
    fn filter(&self) -> anyhow::Result<JobFilter> {
        let state = match &self.state {
            None => None,
            Some(s) => {
                Some(JobState::parse(s).ok_or_else(|| anyhow::anyhow!("unknown state: {s}"))?)
            }
        };
        Ok(JobFilter {
            state,
            queue: self.queue.clone(),
            worker: self.worker.clone(),
            tag: self.tag.clone(),
            q: self.q.clone(),
            error: self.error.clone(),
        })
    }
}

#[derive(Debug, Args, Clone)]
pub struct Targets {
    /// Job ids.
    pub ids: Vec<i64>,
    /// Every job matching the filter instead (needs at least one filter; asks first).
    #[arg(long)]
    pub all: bool,
    #[command(flatten)]
    pub filter: FilterArgs,
    /// Don't ask for confirmation with --all.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// Counts per state, queues (limit, rate, nodes, paused / "no nodes") and nodes.
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// List jobs, newest first.
    List {
        #[command(flatten)]
        filter: FilterArgs,
        #[arg(long, default_value_t = 50)]
        limit: u64,
        /// Only jobs with an id below this (the next page).
        #[arg(long)]
        before: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// One job in full, including every error (newest first).
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Failing jobs grouped by worker and error.
    Errors {
        #[arg(long)]
        json: bool,
    },
    /// Retry jobs (any state but executing/available → available now).
    Retry(Targets),
    /// Cancel jobs that haven't finished.
    Cancel(Targets),
    /// Discard jobs that haven't finished (give up on them).
    Discard(Targets),
    /// Delete jobs (any state but executing).
    Delete(Targets),
    /// Run scheduled or retryable jobs now.
    Run(Targets),
    /// Pause a queue on every node (`--all`: every queue, e.g. during a deploy).
    Pause {
        queue: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Resume a paused queue (`--all`: every paused queue).
    Resume {
        queue: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// The recurring schedule with next and last runs; `--run <key>` runs one now.
    Cron {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the Pruner once (default APP_JOBS_PRUNE_DAYS).
    Prune {
        #[arg(long)]
        days: Option<u32>,
    },
}

/// `app jobs …` from `main.rs`: stdout, and confirmation questions on stdin.
pub async fn run(state: &AppState, command: JobsCommand) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let ask = |question: &str| -> bool {
        print!("{question} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        matches!(line.trim(), "y" | "Y" | "yes")
    };
    execute(state, command, &mut out, ask).await
}

/// The CLI with its output and confirmation injected (tests pass a buffer and a closure).
pub async fn execute(
    state: &AppState,
    command: JobsCommand,
    out: &mut impl Write,
    mut confirm: impl FnMut(&str) -> bool,
) -> anyhow::Result<()> {
    let db = &state.db;
    let now = Utc::now();
    match command {
        JobsCommand::Stats { json } => {
            let stats = admin::stats(db).await?;
            let queues = admin::queues(db, now).await?;
            let nodes = admin::nodes(db, now).await?;
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"stats": stats, "queues": queues, "nodes": nodes})
                    )?
                )?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = stats
                .by_state
                .iter()
                .map(|(s, n)| vec![s.as_str().to_owned(), n.to_string()])
                .collect();
            table(out, &["STATE", "JOBS"], &rows)?;
            writeln!(out)?;
            let rows: Vec<Vec<String>> = queues
                .iter()
                .map(|q| {
                    let status = match (q.paused, q.orphaned()) {
                        (true, _) => "paused",
                        (false, true) => "no nodes",
                        _ => "running",
                    };
                    vec![
                        q.name.clone(),
                        status.to_owned(),
                        q.limit.to_string(),
                        q.rate.clone().unwrap_or_else(|| "-".into()),
                        q.nodes.to_string(),
                        q.executing.to_string(),
                        q.available.to_string(),
                        q.scheduled.to_string(),
                        q.retryable.to_string(),
                        q.discarded.to_string(),
                    ]
                })
                .collect();
            table(
                out,
                &[
                    "QUEUE",
                    "STATUS",
                    "LIMIT",
                    "RATE",
                    "NODES",
                    "EXECUTING",
                    "AVAILABLE",
                    "SCHEDULED",
                    "RETRYABLE",
                    "DISCARDED",
                ],
                &rows,
            )?;
            writeln!(out)?;
            let rows: Vec<Vec<String>> = nodes
                .iter()
                .map(|n| {
                    vec![
                        n.node.clone(),
                        n.queues
                            .iter()
                            .map(|q| format!("{}:{}", q.name, q.limit))
                            .collect::<Vec<_>>()
                            .join(","),
                        n.executing.to_string(),
                        ago(n.heartbeat_at, now),
                        if n.alive { "alive" } else { "silent" }.to_owned(),
                    ]
                })
                .collect();
            table(
                out,
                &["NODE", "QUEUES", "RUNNING", "HEARTBEAT", "STATUS"],
                &rows,
            )?;
        }
        JobsCommand::List {
            filter,
            limit,
            before,
            json,
        } => {
            let jobs = admin::list(db, &filter.filter()?, before, limit).await?;
            if json {
                let shown: Vec<_> = jobs.iter().map(shown).collect();
                writeln!(out, "{}", serde_json::to_string_pretty(&shown)?)?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = jobs
                .iter()
                .map(|j| {
                    vec![
                        j.id.to_string(),
                        j.state.as_str().to_owned(),
                        j.queue.clone(),
                        j.worker.clone(),
                        format!("{}/{}", j.attempt, j.max_attempts),
                        when(j, now),
                        crate::kernel::ui::truncate(&redact(&j.args).to_string(), 60),
                    ]
                })
                .collect();
            table(
                out,
                &["ID", "STATE", "QUEUE", "WORKER", "ATTEMPT", "WHEN", "ARGS"],
                &rows,
            )?;
            if let Some(last) = jobs.last()
                && jobs.len() as u64 == limit
            {
                writeln!(out, "\nolder: app jobs list --before {}", last.id)?;
            }
        }
        JobsCommand::Show { id, json } => {
            let Some(j) = admin::get(db, id).await? else {
                anyhow::bail!("job {id} not found (deleted or pruned)");
            };
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&shown(&j))?)?;
                return Ok(());
            }
            writeln!(out, "job {}  {}  {}", j.id, j.state.as_str(), j.worker)?;
            writeln!(out, "queue      {}", j.queue)?;
            writeln!(out, "attempt    {}/{}", j.attempt, j.max_attempts)?;
            writeln!(out, "priority   {}", j.priority)?;
            writeln!(out, "when       {}", when(&j, now))?;
            writeln!(out, "inserted   {}", j.inserted_at.to_rfc3339())?;
            writeln!(out, "scheduled  {}", j.scheduled_at.to_rfc3339())?;
            if let Some(node) = &j.attempted_by {
                writeln!(out, "node       {node}")?;
            }
            writeln!(out, "tags       {}", j.tag_list().join(", "))?;
            writeln!(
                out,
                "args       {}",
                serde_json::to_string_pretty(&redact(&j.args))?
            )?;
            writeln!(
                out,
                "meta       {}",
                serde_json::to_string_pretty(&redact(&j.meta))?
            )?;
            let errors = j.errors.as_array().cloned().unwrap_or_default();
            writeln!(out, "errors     {}", errors.len())?;
            for e in errors.iter().rev() {
                writeln!(
                    out,
                    "\n  attempt {} at {}{}",
                    e["attempt"],
                    e["at"].as_str().unwrap_or_default(),
                    e["node"]
                        .as_str()
                        .map(|n| format!(" on {n}"))
                        .unwrap_or_default()
                )?;
                for line in e["error"].as_str().unwrap_or_default().lines() {
                    writeln!(out, "    {line}")?;
                }
            }
            if admin::can_retry(j.state) {
                writeln!(out, "\nretry: app jobs retry {}", j.id)?;
            }
        }
        JobsCommand::Errors { json } => {
            let groups = admin::error_groups(db, 5_000).await?;
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&groups)?)?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = groups
                .iter()
                .map(|g| {
                    vec![
                        g.count.to_string(),
                        g.worker.clone(),
                        g.last_seen.map(|t| ago(t, now)).unwrap_or_default(),
                        g.example_id.to_string(),
                        crate::kernel::ui::truncate(&g.message, 80),
                    ]
                })
                .collect();
            table(out, &["JOBS", "WORKER", "LAST", "EXAMPLE", "ERROR"], &rows)?;
        }
        JobsCommand::Retry(t) => act(db, Action::Retry, t, out, &mut confirm).await?,
        JobsCommand::Cancel(t) => act(db, Action::Cancel, t, out, &mut confirm).await?,
        JobsCommand::Discard(t) => act(db, Action::Discard, t, out, &mut confirm).await?,
        JobsCommand::Delete(t) => act(db, Action::Delete, t, out, &mut confirm).await?,
        JobsCommand::Run(t) => act(db, Action::RunNow, t, out, &mut confirm).await?,
        JobsCommand::Pause { queue, all } => match (queue, all) {
            (_, true) => writeln!(out, "paused {} queues", admin::pause_all(db).await?)?,
            (Some(q), false) => {
                admin::pause(db, &q).await?;
                writeln!(out, "paused {q}")?;
            }
            (None, false) => anyhow::bail!("name a queue, or --all"),
        },
        JobsCommand::Resume { queue, all } => match (queue, all) {
            (_, true) => writeln!(out, "resumed {} queues", admin::resume_all(db).await?)?,
            (Some(q), false) => {
                admin::resume(db, &q).await?;
                writeln!(out, "resumed {q}")?;
            }
            (None, false) => anyhow::bail!("name a queue, or --all"),
        },
        JobsCommand::Cron { run, json } => {
            let schedule = super::recurring::SCHEDULE;
            if let Some(key) = run {
                let entry = schedule
                    .iter()
                    .find(|e| e.key == key)
                    .ok_or_else(|| anyhow::anyhow!("no cron entry {key:?}"))?;
                let id = plugins::run_cron_now(db, entry).await?;
                writeln!(out, "enqueued job {id} for {key}")?;
                return Ok(());
            }
            let last = admin::last_runs(db).await?;
            let entries: Vec<_> = schedule
                .iter()
                .map(|e| {
                    let (last_at, last_job) = last.get(e.key).copied().unzip();
                    json!({
                        "key": e.key,
                        "cron": e.cron,
                        "worker": (e.job)().worker_name(),
                        "next_run": next_run(e.cron, now),
                        "last_run": last_at,
                        "last_job": last_job.flatten(),
                    })
                })
                .collect();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&entries)?)?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = schedule
                .iter()
                .map(|e| {
                    let next = next_run(e.cron, now)
                        .map(|t| format!("{} ({})", until(t, now), t.format("%Y-%m-%d %H:%M UTC")))
                        .unwrap_or_else(|| "invalid".into());
                    let last = last
                        .get(e.key)
                        .map(|(t, _)| ago(*t, now))
                        .unwrap_or_else(|| "never".into());
                    vec![
                        e.key.to_owned(),
                        e.cron.to_owned(),
                        (e.job)().worker_name().to_owned(),
                        next,
                        last,
                    ]
                })
                .collect();
            table(out, &["KEY", "CRON (UTC)", "WORKER", "NEXT", "LAST"], &rows)?;
        }
        JobsCommand::Prune { days } => {
            let days = days.unwrap_or(state.config.jobs.prune_days);
            let n = prune(db, now, chrono::Duration::days(i64::from(days))).await?;
            writeln!(out, "pruned {n} jobs finished more than {days} days ago")?;
        }
    }
    Ok(())
}

async fn act(
    db: &DatabaseConnection,
    action: Action,
    t: Targets,
    out: &mut impl Write,
    confirm: &mut impl FnMut(&str) -> bool,
) -> anyhow::Result<()> {
    let n = if t.all {
        let filter = t.filter.filter()?;
        if filter.is_empty() {
            anyhow::bail!(
                "--all needs at least one filter (--state, --queue, --worker, --tag, --q, --error)"
            );
        }
        let count = admin::count_matching(db, &filter).await?;
        let capped = count.min(admin::BULK_CAP);
        if !t.yes && !confirm(&format!("{} {capped} jobs?", action.label())) {
            writeln!(out, "nothing changed")?;
            return Ok(());
        }
        action.apply_matching(db, &filter).await?
    } else {
        if t.ids.is_empty() {
            anyhow::bail!("give job ids, or --all with a filter");
        }
        action.apply(db, &t.ids).await?
    };
    writeln!(out, "{} {n} jobs", action.past().to_lowercase())?;
    Ok(())
}

/// A job as the CLI prints it: args and meta redacted.
fn shown(j: &job::Model) -> serde_json::Value {
    let mut v = serde_json::to_value(j).unwrap_or_default();
    v["args"] = redact(&j.args);
    v["meta"] = redact(&j.meta);
    v
}

/// The time that matters for the job's state.
pub fn when(j: &job::Model, now: chrono::DateTime<Utc>) -> String {
    match j.state {
        JobState::Executing => j
            .attempted_at
            .map(|t| {
                format!(
                    "running for {}",
                    duration((now - t).num_milliseconds().max(0))
                )
            })
            .unwrap_or_default(),
        JobState::Available => format!("waiting since {}", ago(j.scheduled_at, now)),
        JobState::Scheduled => format!("runs {}", until(j.scheduled_at, now)),
        JobState::Retryable => format!("retries {}", until(j.scheduled_at, now)),
        JobState::Completed => j
            .completed_at
            .map(|t| format!("completed {}", ago(t, now)))
            .unwrap_or_default(),
        JobState::Discarded => j
            .discarded_at
            .map(|t| format!("discarded {}", ago(t, now)))
            .unwrap_or_default(),
        JobState::Cancelled => j
            .cancelled_at
            .map(|t| format!("cancelled {}", ago(t, now)))
            .unwrap_or_default(),
    }
}

fn table(out: &mut impl Write, header: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    if rows.is_empty() {
        return writeln!(out, "(none)");
    }
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    writeln!(out, "{}", line(header.to_vec()))?;
    for row in rows {
        writeln!(out, "{}", line(row.iter().map(String::as_str).collect()))?;
    }
    Ok(())
}
