//! `/admin/jobs`: the live jobs dashboard (Oban Web / Mission Control). Server-rendered Askama,
//! vendored htmx and its own `static/kernel/jobs.{css,js}`, identical in every `FRONTEND_MODE`.
//! Everything here is a thin view over `kernel::jobs::admin`, so the browser and `app jobs`
//! can't disagree.
//! GUIDE: .offrails/guides/jobs-dashboard.md
//!
//! SECURITY:
//! - access is the `AdminAccess` route layer on this whole router (`controllers/admin_access.rs`);
//! - every change is a POST, checked by the app's CSRF layer; GET never changes anything;
//! - bulk and destructive actions render a confirmation page first and act only on `confirmed=1`;
//! - redirects go only to paths under `/admin/jobs` (`safe_return_to`);
//! - every response is `Cache-Control: no-store`.
//!
//! (test: get_never_changes_anything, every_dashboard_response_is_no_store)

mod views;

use askama::Template;
use axum::{
    Extension, Router,
    extract::{OriginalUri, Path, Query, State},
    http::HeaderValue,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::Form;
use chrono::Utc;
use serde::Deserialize;

use self::views::{BASE, FilterView, query};
use crate::{
    controllers::application::HxRequest,
    error::AppError,
    kernel::{
        flash::{Flash, RequestFlash},
        jobs::{
            JobState, Recurring,
            admin::{self, Action, JobFilter},
            next_run, plugins,
        },
        theme::ThemeSwitch,
        ui::{self, Chrome},
    },
    state::AppState,
};

/// Rows per page in the job list.
const PAGE_SIZE: u64 = 50;
/// The overview's chart window.
const CHART_MINUTES: i64 = 60;
/// Failing jobs the error page groups (the newest).
const ERROR_SCAN: u64 = 5_000;

/// The app's recurring schedule, handed to the router (the kernel never names app types).
#[derive(Clone, Copy)]
struct Schedule(&'static [Recurring]);

/// Every dashboard route. `routes.rs` mounts it behind `AdminAccess`.
pub fn router(schedule: &'static [Recurring]) -> Router<AppState> {
    Router::new()
        .route("/admin/jobs", get(overview))
        .route("/admin/jobs/list", get(list))
        .route("/admin/jobs/errors", get(errors))
        .route("/admin/jobs/cron", get(cron))
        .route("/admin/jobs/cron/{name}/run", post(cron_run))
        .route("/admin/jobs/bulk", post(bulk))
        .route("/admin/jobs/queues/pause-all", post(pause_all))
        .route("/admin/jobs/queues/resume-all", post(resume_all))
        .route("/admin/jobs/queues/{name}/{action}", post(queue_action))
        .route("/admin/jobs/{id}", get(show))
        .route("/admin/jobs/{id}/{action}", post(job_action))
        .layer(Extension(Schedule(schedule)))
        // WHY: live data: Back must never show a stale snapshot (test: every_dashboard_response_is_no_store)
        .layer(ui::no_store())
}

// ---------------------------------------------------------------------------
// Shared page parts
// ---------------------------------------------------------------------------

/// The dashboard's own header: sub-tabs, live toggle, pause-all switch.
pub struct Nav {
    pub active: &'static str,
    pub live: bool,
    pub live_href: String,
    pub failing: String,
    pub has_failing: bool,
    pub all_paused: bool,
    pub any_queue: bool,
    pub return_to: String,
}

fn chrome(
    state: &AppState,
    title: &str,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Chrome {
    // WHY: the page declares its own stylesheet and script; the kernel layout links exactly
    // these (test: the_dashboard_loads_its_own_css_and_js).
    Chrome::new(
        title,
        "jobs",
        state.assets.assets(),
        state.config.is_development(),
        flash.and_then(|f| f.0.0),
        theme,
    )
    .stylesheet(state.assets.path("kernel/jobs.css"))
    .script(state.assets.path("kernel/jobs.js"))
}

async fn nav(
    state: &AppState,
    active: &'static str,
    live: bool,
    uri: &OriginalUri,
) -> Result<Nav, AppError> {
    let stats = admin::stats(&state.db).await?;
    let queues = admin::queues(&state.db, Utc::now()).await?;
    let failing = stats.count(JobState::Retryable) + stats.count(JobState::Discarded);
    let path_and_query = uri.0.path_and_query().map_or(BASE, |pq| pq.as_str());
    Ok(Nav {
        active,
        live,
        live_href: toggle_live(path_and_query, live),
        failing: views::compact(failing),
        has_failing: failing > 0,
        all_paused: !queues.is_empty() && queues.iter().all(|q| q.paused),
        any_queue: !queues.is_empty(),
        return_to: safe_return_to(path_and_query).unwrap_or_else(|| BASE.to_owned()),
    })
}

/// The same URL with `live=0` added or removed.
fn toggle_live(path_and_query: &str, live: bool) -> String {
    let (path, q) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let mut pairs: Vec<&str> = q
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("live=") && !p.starts_with("part="))
        .collect();
    if live {
        pairs.push("live=0");
    }
    if pairs.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", pairs.join("&"))
    }
}

fn is_live(live: &Option<String>) -> bool {
    live.as_deref() != Some("0")
}

fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(200).collect())
}

/// Same-site paths under `/admin/jobs` only.
/// SECURITY: no open redirects: `//evil.example`, `/admin/jobsevil`, backslashes and control
/// characters are refused (test: return_to_only_accepts_dashboard_paths).
fn safe_return_to(raw: &str) -> Option<String> {
    let ok = raw == BASE || raw.starts_with("/admin/jobs/") || raw.starts_with("/admin/jobs?");
    (ok && !raw.contains('\\') && !raw.contains("//") && !raw.chars().any(char::is_control))
        .then(|| raw.to_owned())
}

fn changed(n: u64, to: String, flash: Flash) -> Response {
    let mut res = (flash, Redirect::to(&to)).into_response();
    if let Ok(v) = HeaderValue::from_str(&n.to_string()) {
        res.headers_mut().insert("x-jobs-changed", v);
    }
    res
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {one}s", ui::thousands(n))
    }
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct PartQuery {
    part: Option<String>,
    live: Option<String>,
}

#[derive(Template)]
#[template(path = "kernel/jobs/index.html")]
struct OverviewPage {
    chrome: Chrome,
    nav: Nav,
    cards: Vec<views::Card>,
    charts: views::Charts,
    running: Vec<views::RunningView>,
    queues: Vec<views::QueueView>,
    workers: Vec<views::WorkerView>,
    nodes: Vec<views::NodeView>,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_stats.html")]
struct StatsPart {
    cards: Vec<views::Card>,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_charts.html")]
struct ChartsPart {
    charts: views::Charts,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_running.html")]
struct RunningPart {
    running: Vec<views::RunningView>,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_queues.html")]
struct QueuesPart {
    queues: Vec<views::QueueView>,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_workers.html")]
struct WorkersPart {
    workers: Vec<views::WorkerView>,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_nodes.html")]
struct NodesPart {
    nodes: Vec<views::NodeView>,
    live: bool,
}

async fn overview(
    State(state): State<AppState>,
    uri: OriginalUri,
    HxRequest(htmx): HxRequest,
    Query(q): Query<PartQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let live = is_live(&q.live);
    let now = Utc::now();
    let db = &state.db;
    let part = q.part.as_deref().filter(|_| htmx);
    Ok(match part {
        Some("stats") => ui::page(StatsPart {
            cards: views::cards(&admin::stats(db).await?),
            live,
        }),
        Some("charts") => ui::page(ChartsPart {
            charts: views::charts(&admin::throughput(db, now, CHART_MINUTES).await?),
            live,
        }),
        Some("running") => ui::page(RunningPart {
            running: views::running(&admin::running(db, 20).await?, now),
            live,
        }),
        Some("queues") => ui::page(QueuesPart {
            queues: views::queues(admin::queues(db, now).await?),
            live,
        }),
        Some("workers") => ui::page(WorkersPart {
            workers: views::workers(
                admin::worker_stats(db, now, chrono::Duration::minutes(CHART_MINUTES)).await?,
            ),
            live,
        }),
        Some("nodes") => ui::page(NodesPart {
            nodes: views::nodes(admin::nodes(db, now).await?, now),
            live,
        }),
        Some(_) => return Err(AppError::NotFound),
        None => ui::page(OverviewPage {
            chrome: chrome(&state, "Jobs", flash, theme),
            nav: nav(&state, "overview", live, &uri).await?,
            cards: views::cards(&admin::stats(db).await?),
            charts: views::charts(&admin::throughput(db, now, CHART_MINUTES).await?),
            running: views::running(&admin::running(db, 20).await?, now),
            queues: views::queues(admin::queues(db, now).await?),
            workers: views::workers(
                admin::worker_stats(db, now, chrono::Duration::minutes(CHART_MINUTES)).await?,
            ),
            nodes: views::nodes(admin::nodes(db, now).await?, now),
            live,
        }),
    })
}

// ---------------------------------------------------------------------------
// Job list
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    state: Option<String>,
    queue: Option<String>,
    worker: Option<String>,
    tag: Option<String>,
    q: Option<String>,
    error: Option<String>,
    before: Option<String>,
    part: Option<String>,
    live: Option<String>,
}

impl ListQuery {
    fn filter(&self) -> JobFilter {
        JobFilter {
            state: self.state.as_deref().and_then(JobState::parse),
            queue: non_empty(&self.queue),
            worker: non_empty(&self.worker),
            tag: non_empty(&self.tag),
            q: non_empty(&self.q),
            error: non_empty(&self.error),
        }
    }
}

#[derive(Template)]
#[template(path = "kernel/jobs/list.html")]
struct ListPage {
    chrome: Chrome,
    nav: Nav,
    filter: FilterView,
    chips: Vec<views::Chip>,
    tabs: Vec<views::StateTab>,
    queue_names: Vec<String>,
    worker_names: Vec<String>,
    rows: Vec<views::RowView>,
    rows_url: String,
    first_page: bool,
    older_href: Option<String>,
    newest_href: String,
    matching: String,
    matching_count: u64,
    over_cap: bool,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_rows.html")]
struct RowsPart {
    rows: Vec<views::RowView>,
    rows_url: String,
    first_page: bool,
    filter: FilterView,
    live: bool,
}

async fn list(
    State(state): State<AppState>,
    uri: OriginalUri,
    HxRequest(htmx): HxRequest,
    Query(q): Query<ListQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let live = is_live(&q.live);
    let now = Utc::now();
    let db = &state.db;
    let filter = q.filter();
    let before = q.before.as_deref().and_then(|b| b.parse::<i64>().ok());
    let jobs = admin::list(db, &filter, before, PAGE_SIZE).await?;
    let fv = FilterView::new(&filter);
    let rows_url = format!(
        "{BASE}/list{}",
        query(&[
            ("state", &fv.state),
            ("queue", &fv.queue),
            ("worker", &fv.worker),
            ("tag", &fv.tag),
            ("q", &fv.q),
            ("error", &fv.error),
            ("part", "rows"),
        ])
    );
    let rows = views::rows(&jobs, now);
    if htmx && q.part.as_deref() == Some("rows") {
        return Ok(ui::page(RowsPart {
            rows,
            rows_url,
            first_page: before.is_none(),
            filter: fv,
            live,
        }));
    }

    let counts = admin::state_counts(db, &filter).await?;
    let total: u64 = counts.values().sum();
    let mut tabs = vec![views::StateTab {
        label: "All",
        state: "",
        count: views::compact(total),
        href: format!("{BASE}/list{}", query(&fv.pairs_with_state(""))),
        active: fv.state.is_empty(),
    }];
    for s in JobState::ALL {
        tabs.push(views::StateTab {
            label: views::state_label(s),
            state: s.as_str(),
            count: views::compact(counts.get(&s).copied().unwrap_or(0)),
            href: format!("{BASE}/list{}", query(&fv.pairs_with_state(s.as_str()))),
            active: fv.state == s.as_str(),
        });
    }
    let matching_count = if filter.is_empty() {
        0
    } else {
        admin::count_matching(db, &filter).await?
    };
    let older_href = (jobs.len() as u64 == PAGE_SIZE).then(|| {
        let last = jobs.last().map(|j| j.id.to_string()).unwrap_or_default();
        let mut pairs = fv.pairs_with_state(&fv.state);
        pairs.push(("before", &last));
        format!("{BASE}/list{}", query(&pairs))
    });
    let queue_names = admin::queues(db, now)
        .await?
        .into_iter()
        .map(|q| q.name)
        .collect();
    Ok(ui::page(ListPage {
        chrome: chrome(&state, "Job list", flash, theme),
        nav: nav(&state, "list", live, &uri).await?,
        chips: views::chips(&fv),
        newest_href: fv.href(),
        filter: fv,
        tabs,
        queue_names,
        worker_names: admin::workers(db).await?,
        rows,
        rows_url,
        first_page: before.is_none(),
        older_href,
        matching: ui::thousands(matching_count.min(admin::BULK_CAP)),
        over_cap: matching_count > admin::BULK_CAP,
        matching_count,
        live,
    }))
}

// ---------------------------------------------------------------------------
// Job page
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "kernel/jobs/show.html")]
struct ShowPage {
    chrome: Chrome,
    nav: Nav,
    job: views::JobView,
    live: bool,
}

#[derive(Template)]
#[template(path = "kernel/jobs/_job_status.html")]
struct JobStatusPart {
    job: views::JobView,
    live: bool,
}

async fn show(
    State(state): State<AppState>,
    uri: OriginalUri,
    HxRequest(htmx): HxRequest,
    Path(id): Path<String>,
    Query(q): Query<PartQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let live = is_live(&q.live);
    let Ok(id) = id.parse::<i64>() else {
        return Err(AppError::NotFound);
    };
    let Some(job) = admin::get(&state.db, id).await? else {
        // Deleted, pruned, or reached with Back after a delete.
        return Ok((
            Flash::alert(format!("Job {id} no longer exists (deleted or pruned).")),
            Redirect::to(&format!("{BASE}/list")),
        )
            .into_response());
    };
    let batch = match job.batch_id {
        Some(b) => admin::batch(&state.db, b).await?,
        None => None,
    };
    let view = views::job_view(&job, batch, Utc::now());
    if htmx && q.part.as_deref() == Some("status") {
        return Ok(ui::page(JobStatusPart { job: view, live }));
    }
    Ok(ui::page(ShowPage {
        chrome: chrome(&state, &format!("Job {id}"), flash, theme),
        nav: nav(&state, "list", live, &uri).await?,
        job: view,
        live,
    }))
}

// ---------------------------------------------------------------------------
// Errors and cron
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct ErrorsQuery {
    worker: Option<String>,
    live: Option<String>,
}

#[derive(Template)]
#[template(path = "kernel/jobs/errors.html")]
struct ErrorsPage {
    chrome: Chrome,
    nav: Nav,
    groups: Vec<views::ErrorGroupView>,
    worker: String,
    clear_href: String,
}

async fn errors(
    State(state): State<AppState>,
    uri: OriginalUri,
    Query(q): Query<ErrorsQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let worker = non_empty(&q.worker).unwrap_or_default();
    let mut groups = admin::error_groups(&state.db, ERROR_SCAN).await?;
    if !worker.is_empty() {
        groups.retain(|g| g.worker == worker);
    }
    Ok(ui::page(ErrorsPage {
        chrome: chrome(&state, "Errors", flash, theme),
        nav: nav(&state, "errors", is_live(&q.live), &uri).await?,
        groups: views::error_groups(groups, Utc::now()),
        worker,
        clear_href: format!("{BASE}/errors"),
    }))
}

#[derive(Template)]
#[template(path = "kernel/jobs/cron.html")]
struct CronPage {
    chrome: Chrome,
    nav: Nav,
    entries: Vec<views::CronView>,
}

async fn cron(
    State(state): State<AppState>,
    uri: OriginalUri,
    Extension(Schedule(schedule)): Extension<Schedule>,
    Query(q): Query<PartQuery>,
    flash: Option<Extension<RequestFlash>>,
    theme: ThemeSwitch,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let last = admin::last_runs(&state.db).await?;
    let entries = schedule
        .iter()
        .map(|e| {
            let job = (e.job)();
            let next = next_run(e.cron, now);
            let (last_at, last_job) = last.get(e.key).copied().unzip();
            views::CronView {
                key: e.key.to_owned(),
                cron: e.cron.to_owned(),
                description: plugins::describe_cron(e.cron)
                    .unwrap_or_else(|| "invalid expression".into()),
                worker: job.worker_name().to_owned(),
                worker_href: format!("{BASE}/list{}", query(&[("worker", job.worker_name())])),
                queue: job.queue_name().to_owned(),
                next: next.map(|t| ui::until(t, now)).unwrap_or_default(),
                next_utc: next.map(ui::utc).unwrap_or_default(),
                last: last_at
                    .map(|t| ui::ago(t, now))
                    .unwrap_or_else(|| "never".into()),
                last_href: last_job
                    .flatten()
                    .map(|id| format!("{BASE}/{id}"))
                    .unwrap_or_default(),
                run_url: format!("{BASE}/cron/{}/run", crate::kernel::cookie::encode(e.key)),
                valid: next.is_some(),
            }
        })
        .collect();
    Ok(ui::page(CronPage {
        chrome: chrome(&state, "Cron", flash, theme),
        nav: nav(&state, "cron", is_live(&q.live), &uri).await?,
        entries,
    }))
}

// ---------------------------------------------------------------------------
// Actions (POST only; the CSRF layer checks every one)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "kernel/jobs/confirm.html")]
struct ConfirmPage {
    chrome: Chrome,
    title: String,
    body: String,
    action_url: String,
    fields: Vec<(String, String)>,
    button: String,
    danger: bool,
    cancel_href: String,
}

/// What a confirmation page asks.
struct Confirm {
    title: String,
    body: String,
    action_url: String,
    fields: Vec<(String, String)>,
    button: String,
    danger: bool,
    cancel_href: String,
}

/// Renders the confirmation: the same POST again, with every field and `confirmed=1`.
fn confirm_page(state: &AppState, theme: ThemeSwitch, c: Confirm) -> Response {
    // WHY: this page answers a POST; the theme switch's no-JS fallback comes back with a GET,
    // so it returns to where Cancel goes.
    let theme = ThemeSwitch {
        return_to: c.cancel_href.clone(),
        ..theme
    };
    let mut fields: Vec<(String, String)> = c
        .fields
        .into_iter()
        .filter(|(k, _)| k != "confirmed")
        .collect();
    fields.push(("confirmed".into(), "1".into()));
    ui::page(ConfirmPage {
        chrome: chrome(state, "Confirm", None, theme),
        title: c.title,
        body: c.body,
        action_url: c.action_url,
        fields,
        button: c.button,
        danger: c.danger,
        cancel_href: c.cancel_href,
    })
}

fn field<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// `POST /admin/jobs/{id}/{action}`: retry, run, cancel, discard, delete. Cancel, discard and
/// delete ask first.
async fn job_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    theme: ThemeSwitch,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let (Ok(id), Some(action)) = (id.parse::<i64>(), Action::parse(&action)) else {
        return Err(AppError::NotFound);
    };
    let return_to = field(&form, "return_to")
        .and_then(safe_return_to)
        .unwrap_or_else(|| match action {
            Action::Delete => format!("{BASE}/list"),
            _ => format!("{BASE}/{id}"),
        });
    let needs_confirm = matches!(action, Action::Cancel | Action::Discard | Action::Delete);
    if needs_confirm && field(&form, "confirmed") != Some("1") {
        let Some(job) = admin::get(&state.db, id).await? else {
            return Ok(changed(
                0,
                format!("{BASE}/list"),
                Flash::alert(format!("Job {id} no longer exists (deleted or pruned).")),
            ));
        };
        let what = format!("job {id} ({})", job.worker);
        let body = match action {
            Action::Delete => format!("This removes {what} and its error history for good."),
            Action::Discard => {
                format!("{what} stops retrying and is marked discarded. You can retry it later.")
            }
            _ => {
                format!("{what} is cancelled; if it is running, its node stops it within a second.")
            }
        };
        return Ok(confirm_page(
            &state,
            theme,
            Confirm {
                title: format!("{} job {id}?", action.label()),
                body,
                action_url: format!("{BASE}/{id}/{}", action.as_str()),
                fields: vec![("return_to".into(), return_to)],
                button: format!("{} job {id}", action.label()),
                danger: action != Action::Cancel,
                cancel_href: format!("{BASE}/{id}"),
            },
        ));
    }
    let n = action.apply(&state.db, &[id]).await?;
    let flash = if n == 0 {
        Flash::alert("Nothing changed: the job's state doesn't allow that.")
    } else {
        Flash::notice(format!("{} job {id}.", action.past()))
    };
    Ok(changed(n, return_to, flash))
}

/// `POST /admin/jobs/bulk`: `action` + `ids`, or `all_matching=1` + the filter fields. Always
/// asks first, with the exact count. "All matching" buttons carry their action in the query
/// (`formaction="/admin/jobs/bulk?action=retry"`), so query and form pairs are merged.
async fn bulk(
    State(state): State<AppState>,
    uri: OriginalUri,
    theme: ThemeSwitch,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let mut pairs: Vec<(String, String)> = uri
        .0
        .query()
        .map(|q| serde_html_form::from_str::<Vec<(String, String)>>(q).unwrap_or_default())
        .unwrap_or_default();
    pairs.extend(form);
    let Some(action) = field(&pairs, "action").and_then(Action::parse) else {
        return Ok(changed(
            0,
            format!("{BASE}/list"),
            Flash::alert("Pick an action."),
        ));
    };
    let f = |name: &str| {
        field(&pairs, name)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.chars().take(200).collect::<String>())
    };
    let filter = JobFilter {
        state: f("state").as_deref().and_then(JobState::parse),
        queue: f("queue"),
        worker: f("worker"),
        tag: f("tag"),
        q: f("q"),
        error: f("error"),
    };
    let return_to = f("return_to")
        .as_deref()
        .and_then(safe_return_to)
        .unwrap_or_else(|| FilterView::new(&filter).href());
    let all_matching = field(&pairs, "all_matching") == Some("1");
    let ids: Vec<i64> = pairs
        .iter()
        .filter(|(k, _)| k == "ids")
        .filter_map(|(_, v)| v.parse().ok())
        .take(admin::BULK_CAP as usize)
        .collect();

    let count = if all_matching {
        // SECURITY: an empty filter would mean "every job"; it's refused, never confirmed
        // (test: bulk_actions_confirm_with_the_exact_count_and_refuse_an_empty_filter).
        if filter.is_empty() {
            return Ok(changed(
                0,
                return_to,
                Flash::alert("Set a filter first: bulk actions never apply to every job."),
            ));
        }
        admin::count_matching(&state.db, &filter)
            .await?
            .min(admin::BULK_CAP)
    } else {
        if ids.is_empty() {
            return Ok(changed(
                0,
                return_to,
                Flash::alert("Select some jobs first."),
            ));
        }
        ids.len() as u64
    };

    if field(&pairs, "confirmed") != Some("1") {
        let scope = if all_matching {
            let fv = FilterView::new(&filter);
            let mut parts = Vec::new();
            for (label, v) in [
                ("state", &fv.state),
                ("queue", &fv.queue),
                ("worker", &fv.worker),
                ("tag", &fv.tag),
                ("search", &fv.q),
                ("error", &fv.error),
            ] {
                if !v.is_empty() {
                    parts.push(format!("{label} “{v}”"));
                }
            }
            format!(
                "Every job matching {} (up to {} at a time). Jobs whose state doesn't allow it are skipped.",
                parts.join(", "),
                ui::thousands(admin::BULK_CAP)
            )
        } else {
            "The selected jobs. Jobs whose state doesn't allow it are skipped.".to_owned()
        };
        let mut fields: Vec<(String, String)> = pairs
            .iter()
            .filter(|(k, _)| k != "confirmed")
            .cloned()
            .collect();
        if !fields.iter().any(|(k, _)| k == "return_to") {
            fields.push(("return_to".into(), return_to.clone()));
        }
        return Ok(confirm_page(
            &state,
            theme,
            Confirm {
                title: format!("{} {}?", action.label(), plural(count, "job")),
                body: scope,
                action_url: format!("{BASE}/bulk"),
                fields,
                button: format!("{} {}", action.label(), plural(count, "job")),
                danger: matches!(action, Action::Delete | Action::Discard | Action::Cancel),
                cancel_href: return_to,
            },
        ));
    }

    let n = if all_matching {
        action.apply_matching(&state.db, &filter).await?
    } else {
        action.apply(&state.db, &ids).await?
    };
    let flash = if n == 0 {
        Flash::alert("Nothing changed: no selected job was in a state that allows that.")
    } else {
        Flash::notice(format!("{} {}.", action.past(), plural(n, "job")))
    };
    Ok(changed(n, return_to, flash))
}

/// `POST /admin/jobs/queues/{name}/{action}`: pause / resume one queue on every node.
async fn queue_action(
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    if !crate::kernel::jobs::valid_queue_name(&name) {
        return Err(AppError::NotFound);
    }
    let return_to = field(&form, "return_to")
        .and_then(safe_return_to)
        .unwrap_or_else(|| BASE.to_owned());
    match action.as_str() {
        "pause" => admin::pause(&state.db, &name).await?,
        "resume" => admin::resume(&state.db, &name).await?,
        _ => return Err(AppError::NotFound),
    }
    let verb = if action == "pause" {
        "Paused"
    } else {
        "Resumed"
    };
    Ok(changed(
        1,
        return_to,
        Flash::notice(format!("{verb} queue {name} on every node.")),
    ))
}

/// `POST /admin/jobs/queues/pause-all`: the deploy switch. Asks first.
async fn pause_all(
    State(state): State<AppState>,
    theme: ThemeSwitch,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let return_to = field(&form, "return_to")
        .and_then(safe_return_to)
        .unwrap_or_else(|| BASE.to_owned());
    if field(&form, "confirmed") != Some("1") {
        let n = admin::queues(&state.db, Utc::now()).await?.len() as u64;
        return Ok(confirm_page(
            &state,
            theme,
            Confirm {
                title: format!("Pause all {}?", plural(n, "queue")),
                body: "No node starts new jobs until you resume. Running jobs finish normally; inserts still work. Use it around deploys and migrations.".into(),
                action_url: format!("{BASE}/queues/pause-all"),
                fields: vec![("return_to".into(), return_to.clone())],
                button: "Pause everything".into(),
                danger: true,
                cancel_href: return_to,
            },
        ));
    }
    let n = admin::pause_all(&state.db).await? as u64;
    Ok(changed(
        n,
        return_to,
        Flash::notice(format!("Paused {}.", plural(n, "queue"))),
    ))
}

/// `POST /admin/jobs/queues/resume-all`.
async fn resume_all(
    State(state): State<AppState>,
    Form(form): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let return_to = field(&form, "return_to")
        .and_then(safe_return_to)
        .unwrap_or_else(|| BASE.to_owned());
    let n = admin::resume_all(&state.db).await?;
    Ok(changed(
        n,
        return_to,
        Flash::notice(format!("Resumed {}.", plural(n, "queue"))),
    ))
}

/// `POST /admin/jobs/cron/{name}/run`: enqueue a cron entry's job now.
async fn cron_run(
    State(state): State<AppState>,
    Extension(Schedule(schedule)): Extension<Schedule>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let Some(entry) = schedule.iter().find(|e| e.key == name) else {
        return Err(AppError::NotFound);
    };
    match plugins::run_cron_now(&state.db, entry).await {
        Ok(id) => Ok(changed(
            1,
            format!("{BASE}/{id}"),
            Flash::notice(format!("Enqueued {} now (job {id}).", entry.key)),
        )),
        Err(sea_orm::DbErr::Custom(msg)) => {
            Ok(changed(0, format!("{BASE}/cron"), Flash::alert(msg)))
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_only_accepts_dashboard_paths() {
        for ok in [
            "/admin/jobs",
            "/admin/jobs/list?state=discarded",
            "/admin/jobs/42",
            "/admin/jobs?live=0",
        ] {
            assert_eq!(safe_return_to(ok).as_deref(), Some(ok));
        }
        for bad in [
            "https://evil.example/admin/jobs",
            "//evil.example",
            "/admin/jobsevil",
            "/admin/jobs/..//evil.example",
            "/admin/jobs/\\evil",
            "/admin/jobs/\nx",
            "/",
            "",
        ] {
            assert_eq!(safe_return_to(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_live_toggle_keeps_the_rest_of_the_url() {
        assert_eq!(
            toggle_live("/admin/jobs/list?state=discarded", true),
            "/admin/jobs/list?state=discarded&live=0"
        );
        assert_eq!(
            toggle_live("/admin/jobs/list?state=discarded&live=0", false),
            "/admin/jobs/list?state=discarded"
        );
        assert_eq!(toggle_live("/admin/jobs", true), "/admin/jobs?live=0");
    }
}
