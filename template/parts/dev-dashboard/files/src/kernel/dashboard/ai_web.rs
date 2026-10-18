//! Ask AI's routes and views, under both dashboards (`/dev/dashboard/ai…`, and the read-only
//! `/admin/dashboard/ai…` when `DASHBOARD_AI_IN_PRODUCTION=true`), plus the Ask AI bar that other
//! kernel pages show in development (the mailbox, the jobs dashboard, sockets, `/dev`).
//!
//! | Route | Does |
//! |-------|------|
//! | `GET {base}/ai?tab=&item=&q=` | the assistant on a page of its own (the no-JavaScript path) |
//! | `GET {base}/ai/panel?tab=&item=` or `?page=<path>` | the panel fragment (preview, prompts, form) |
//! | `POST {base}/ai/runs` | starts a run, answers its live fragment (JavaScript) or redirects to it |
//! | `POST {base}/ai/ask` | starts a run and waits for it (no JavaScript; the page shows the result) |
//! | `GET {base}/ai/runs/{id}` | a run's page (live or from the history) |
//! | `GET {base}/ai/runs/{id}/stream` | the run's steps and end as Server-Sent Events (its browser only) |
//! | `GET {base}/ai/history` | past runs |
//! | `GET {base}/explain` | "What am I looking at?" (no key needed) |
//!
//! Every POST goes through the app's CSRF layer; every response is `no-store` (the routers).

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use futures::Stream;
use serde::Deserialize;

use super::ai::{self, Tab};
use super::ai_agent::{self, Kind, Record, Status, Step};
use super::ai_tools::Action;
use super::charts::ChartView;
use super::web::{Dash, Mode, chrome, dash, micros};
use super::{ai_context, ai_explain, ai_watch, http, lock};
use crate::error::AppError;
use crate::kernel::cookie;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui::{self, Chrome};
use crate::state::AppState;

/// How long the no-JavaScript path waits for a run before showing it as it stands (with a link
/// that waits again).
/// WHY: under the app's 30 s request timeout (routes.rs answers 503 past it); the run itself
/// goes on in the background either way.
pub const NO_JS_WAIT: Duration = Duration::from_secs(25);

/// The Ask AI routes under `base` (`read_routes` merges them into both dashboards).
pub fn routes(base: &'static str) -> Router<AppState> {
    let p = |s: &str| format!("{base}{s}");
    Router::new()
        .route(&p("/ai"), get(ai_page))
        .route(&p("/ai/panel"), get(ai_panel))
        .route(&p("/ai/ask"), post(ai_ask))
        .route(&p("/ai/runs"), post(ai_runs))
        .route(&p("/ai/runs/{id}"), get(run_page))
        .route(&p("/ai/runs/{id}/stream"), get(run_stream))
        .route(&p("/ai/runs/{id}/continue"), post(run_continue))
        .route(&p("/ai/history"), get(history_page))
}

// ---- the Ask AI bar on other kernel pages ------------------------------------------------------

/// On in development (set by `instrument`): the kernel layout's wire asks `bar()`.
pub static BAR: AtomicBool = AtomicBool::new(false);

/// The bar for a kernel page outside the dashboard.
pub struct Bar {
    pub tab: &'static str,
    pub label: &'static str,
    /// The panel fragment for this page, and the no-JavaScript page.
    pub src: String,
    pub href: String,
    pub css: String,
    pub js: String,
}

/// `Some` on the kernel pages the assistant knows (the mailbox and a message, the jobs
/// dashboard and a job, sockets, `/dev`, the host's routes and system pages), in development.
/// The dashboard's own pages have their panel already.
pub fn bar(path_and_query: &str) -> Option<Bar> {
    if !BAR.load(Ordering::Relaxed)
        || path_and_query.starts_with("/dev/dashboard")
        || path_and_query.starts_with("/admin/dashboard")
    {
        return None;
    }
    let (tab, item) = Tab::for_page(path_and_query)?;
    let page = cookie::encode(path_and_query.split('#').next().unwrap_or_default());
    let item = item.unwrap_or_default();
    Some(Bar {
        tab: tab.key(),
        label: tab.label(),
        src: format!("/dev/dashboard/ai/panel?page={page}"),
        href: format!("/dev/dashboard/ai?tab={}&item={item}", tab.key()),
        css: asset("kernel/dashboard-ai.css"),
        js: asset("kernel/dashboard-ai.js"),
    })
}

/// `/static/<key>?v=<digest>` (development only, so hashing the one small file is fine).
pub fn asset(key: &str) -> String {
    use sha2::{Digest, Sha256};
    match std::fs::read(std::path::Path::new("static").join(key)) {
        Ok(bytes) => {
            let d = Sha256::digest(&bytes);
            let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
            format!("/static/{key}?v={hex}")
        }
        Err(_) => format!("/static/{key}"),
    }
}

// ---- views -------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AiQuery {
    tab: Option<String>,
    item: Option<String>,
    /// A question to fill in ("Explain more with AI").
    q: Option<String>,
    /// A kernel page's path (the Ask AI bar): resolved with `Tab::for_page`.
    page: Option<String>,
    wait: Option<String>,
    /// The run stream's first step to send (the ones before are on the page).
    from: Option<usize>,
}

/// The panel: how to enable it, or the prepared first request's preview, the prompts, the form.
pub struct AiPanel {
    pub configured: bool,
    pub base: &'static str,
    pub tab: &'static str,
    pub tab_label: &'static str,
    pub item: String,
    /// The item in words ("N+1 on /orders", "the lab_orders table"), never a raw id alone.
    pub item_title: String,
    pub provider: &'static str,
    pub model: String,
    pub endpoint: String,
    pub prepared: String,
    pub headers: Vec<String>,
    /// The exact JSON body of the first call, with the question as a placeholder.
    pub preview: String,
    pub chars: usize,
    pub tokens: usize,
    pub truncated: bool,
    pub suggestions: &'static [&'static str],
    pub question: String,
    pub left: String,
    pub error: Option<String>,
    /// The tools the agent may call (name, title).
    pub tools: Vec<(&'static str, &'static str)>,
    pub max_steps: usize,
}

/// A proposed action as the page shows it: a POST form (CSRF applies) or a link.
pub struct ActionView {
    pub label: String,
    pub reason: String,
    /// The form's action, its field and value; empty for a link.
    pub post: String,
    pub field: &'static str,
    pub value: String,
    pub href: String,
}

/// A run, for `_ai_run.html` (live or finished).
pub struct RunView {
    pub id: String,
    pub base: String,
    pub live: bool,
    /// The live stream's URL (empty when finished).
    pub stream: String,
    pub page: String,
    pub kind: &'static str,
    pub question: String,
    pub status: &'static str,
    pub status_text: String,
    pub steps: Vec<StepView>,
    pub blocks: Vec<ai::Block>,
    pub note: String,
    pub error: Option<String>,
    pub usage: String,
    pub actions: Vec<ActionView>,
    pub fix_prompt: String,
    pub sent: String,
    pub when: String,
    /// The answer was cut and the run is in memory: the Continue button shows.
    pub can_continue: bool,
}

impl RunView {
    /// A view of a failure before any run existed (no provider, an expired preview…).
    pub fn error(base: &str, msg: String) -> Self {
        RunView {
            id: String::new(),
            base: base.to_owned(),
            live: false,
            stream: String::new(),
            page: String::new(),
            kind: "",
            question: String::new(),
            status: "failed",
            status_text: String::new(),
            steps: Vec::new(),
            blocks: Vec::new(),
            note: String::new(),
            error: Some(msg),
            usage: String::new(),
            actions: Vec::new(),
            fix_prompt: String::new(),
            sent: String::new(),
            when: String::new(),
            can_continue: false,
        }
    }
}

/// A step as the page shows it: what the model said before the call goes through the same
/// renderer as the answer (escaped Markdown, citations as chips), never as raw text.
pub struct StepView {
    pub n: usize,
    pub title: String,
    pub call: String,
    pub note: Vec<ai::Block>,
    pub result: String,
    pub is_error: bool,
    pub ms: u64,
}

pub fn step_view(s: &Step, base: &str) -> StepView {
    let base = if base.is_empty() {
        "/dev/dashboard"
    } else {
        base
    };
    StepView {
        n: s.n,
        title: s.title.clone(),
        call: s.call.clone(),
        note: ai::render_in(&s.note, base, chrono::Utc::now().timestamp()),
        result: s.result.clone(),
        is_error: s.is_error,
        ms: s.ms,
    }
}

/// The chart a `[chart:…]` citation draws: the Charts page's chart for that window.
pub fn draw_chart(key: &'static str, from: i64, to: i64) -> Option<ChartView> {
    let (win, _) = super::web::window(super::series::Range::M15, Some(from), Some(to));
    super::web::all_charts(&win, false)
        .into_iter()
        .find(|c| c.key == key)
}

/// An answer's blocks, with citations linked for `base` and up to two charts drawn.
pub fn answer_blocks(text: &str, base: &str) -> Vec<ai::Block> {
    ai::attach_charts(
        ai::render_in(text, base, chrono::Utc::now().timestamp()),
        2,
        draw_chart,
    )
}

fn action_view(a: &Action, base: &str) -> Option<ActionView> {
    let ro = base.starts_with("/admin");
    let v = match a.kind.as_str() {
        "run_scenario" if !ro => ActionView {
            label: a.label(),
            reason: a.reason.clone(),
            post: "/dev/dashboard/lab/scenario".into(),
            field: "scenario",
            value: a.target.clone(),
            href: String::new(),
        },
        "run_tests" if !ro => ActionView {
            label: a.label(),
            reason: a.reason.clone(),
            post: "/dev/dashboard/tests/run".into(),
            field: "db",
            value: a.target.clone(),
            href: String::new(),
        },
        "open_page" if super::ai_tools::action_path_ok(&a.target, ro) => ActionView {
            label: a.label(),
            reason: a.reason.clone(),
            post: String::new(),
            field: "",
            value: String::new(),
            href: a.target.clone(),
        },
        _ => return None,
    };
    Some(v)
}

pub fn run_view(rec: &Record, live: bool) -> RunView {
    let base = if rec.base.is_empty() {
        "/dev/dashboard".to_owned()
    } else {
        rec.base.clone()
    };
    let (status_text, error) = match &rec.status {
        Status::Running => ("Investigating…".to_owned(), None),
        Status::Done => (format!("Answered in {} calls", rec.calls), None),
        Status::Stopped(why) => (format!("Stopped: {why}"), None),
        Status::Failed(why) => ("Failed".to_owned(), Some(why.clone())),
    };
    RunView {
        id: rec.id.clone(),
        live,
        // `from`: the steps the page already shows, so the stream doesn't repeat them.
        stream: if live {
            format!("{base}/ai/runs/{}/stream?from={}", rec.id, rec.steps.len())
        } else {
            String::new()
        },
        page: format!("{base}/ai/runs/{}", rec.id),
        kind: rec.kind.label(),
        // The health question is long and fixed; the page names it.
        question: if rec.kind == Kind::Health {
            "How healthy is this app right now? (the verdict, what's healthy, what needs attention, what to fix first, with evidence)".to_owned()
        } else {
            rec.question.clone()
        },
        status: rec.status.key(),
        status_text,
        steps: rec.steps.iter().map(|s| step_view(s, &base)).collect(),
        blocks: answer_blocks(&rec.answer, &base),
        note: rec.note.clone(),
        error,
        usage: if rec.calls == 0 {
            String::new()
        } else {
            format!(
                "{} · {} · {} calls · {} in, {} out · {}",
                rec.provider,
                rec.model,
                rec.calls,
                ui::thousands(rec.input_tokens),
                ui::thousands(rec.output_tokens),
                lock(&ai::BUDGET).left(ai::today())
            )
        },
        actions: rec
            .actions
            .iter()
            .filter_map(|a| action_view(a, &base))
            .collect(),
        fix_prompt: ai_agent::fix_prompt(rec).unwrap_or_default(),
        sent: rec.sent.clone(),
        when: ui::ago(rec.started, chrono::Utc::now()),
        can_continue: !live && rec.cut && ai_agent::get(&rec.id).is_some_and(|r| r.can_continue()),
        base,
    }
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_ai_panel.html")]
struct AiPanelFragment {
    p: AiPanel,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_ai_run.html")]
struct RunFragment {
    r: RunView,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_ai_step.html")]
struct StepFragment {
    s: StepView,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/_ai_run_end.html")]
struct EndFragment {
    r: RunView,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/ai.html")]
struct AiPage {
    chrome: Chrome,
    d: Dash,
    p: AiPanel,
    r: Option<RunView>,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/ai_run.html")]
struct RunPage {
    chrome: Chrome,
    d: Dash,
    r: RunView,
}

pub struct HistoryRow {
    pub id: String,
    pub kind: &'static str,
    pub tab: String,
    pub question: String,
    pub status: &'static str,
    pub status_text: String,
    pub when: String,
    pub calls: u32,
    pub tokens: String,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/ai_history.html")]
struct HistoryPage {
    chrome: Chrome,
    d: Dash,
    rows: Vec<HistoryRow>,
    keep: usize,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/explain.html")]
struct ExplainPage {
    chrome: Chrome,
    d: Dash,
    groups: Vec<(&'static str, Vec<&'static ai_explain::Part>)>,
}

/// "Anthropic · claude-sonnet-5-5 · https://api.anthropic.com · key: set" for Config.
// SECURITY: the key shows as "set" or "not set", never its value (test: the_ai_key_never_reaches_logs_traces_pages_or_recorded_requests)
pub fn ai_setting(ai: &crate::config::DashboardAi) -> String {
    if ai.provider.is_none() {
        return "off (DASHBOARD_AI_PROVIDER not set)".into();
    }
    format!(
        "{} · {} · {} · key: {}{}{}{}",
        provider_label(ai),
        ai.model,
        ai.base_url,
        if ai.api_key.is_some() {
            "set"
        } else {
            "not set"
        },
        if ai.tools_core { " · core tools" } else { "" },
        if ai.proactive { " · proactive" } else { "" },
        if ai.in_production {
            " · also in production"
        } else {
            ""
        }
    )
}

/// A question placeholder in the preview; the script fills in what is typed.
pub const QUESTION_SLOT: &str = "{your question}";

fn provider_label(ai: &crate::config::DashboardAi) -> &'static str {
    match ai.provider {
        Some(crate::config::AiProvider::Anthropic) => "Anthropic",
        Some(crate::config::AiProvider::OpenAiCompatible) => "OpenAI-compatible",
        None => "not configured",
    }
}

fn tab_and_item(
    q_tab: Option<&str>,
    q_item: Option<&str>,
) -> Result<(Tab, Option<String>), AppError> {
    let tab = q_tab.and_then(Tab::parse).ok_or(AppError::NotFound)?;
    let item = q_item.map(str::trim).filter(|i| !i.is_empty());
    match item {
        Some(i) if !tab.valid_item(i) => Err(AppError::NotFound),
        Some(i) => Ok((tab, Some(i.to_owned()))),
        None => Ok((tab, None)),
    }
}

/// The tab and item a query names: `page=<path>` (the bar) or `tab=&item=`.
fn resolve(q: &AiQuery, default_tab: Option<&str>) -> Result<(Tab, Option<String>), AppError> {
    if let Some(page) = q.page.as_deref() {
        return Tab::for_page(page).ok_or(AppError::NotFound);
    }
    tab_and_item(q.tab.as_deref().or(default_tab), q.item.as_deref())
}

/// The first request's body for a prepared context (the preview, and the first call).
pub fn first_body(
    state: &AppState,
    mode: Mode,
    p: &ai::Prepared,
    question: &str,
) -> serde_json::Value {
    let cfg = &state.config.dashboard.ai;
    let tools = super::ai_tools::enabled(mode.ro, cfg.tools_core);
    ai::cached(
        cfg,
        ai::request(
            cfg,
            &p.system,
            &[ai::user_turn(&ai::user_message(p, question))],
            &super::ai_tools::definitions(cfg.provider, &tools),
            ai::ToolChoice::Auto,
        ),
        false,
    )
}

/// What a page's item is, in words, for the panel's and the page's heading.
pub async fn item_title(state: &AppState, tab: Tab, item: &str) -> String {
    let t = match tab {
        Tab::Insights => super::web::all_findings(state, &Default::default())
            .await
            .into_iter()
            .find(|f| f.id == item)
            .map_or_else(|| "a finding".to_owned(), |f| f.title),
        Tab::Sql => lock(&super::HUB.sql)
            .rows(super::sql::Sort::Count)
            .into_iter()
            .find(|r| r.id == item)
            .map_or_else(
                || "a query".to_owned(),
                |r| format!("the query {}", ui::truncate(&r.sql, 60)),
            ),
        Tab::Traces => format!("trace {item}"),
        Tab::Schema => format!("the {item} table"),
        Tab::Mailbox => format!("email {item}"),
        Tab::Jobs if item == "errors" => "the job errors".to_owned(),
        Tab::Jobs => format!("job {item}"),
        _ => item.to_owned(),
    };
    ui::truncate(&t, 90)
}

/// Builds the context for `tab`, stores it for the Ask button; the panel shows its preview.
pub async fn panel_for(
    state: &AppState,
    mode: Mode,
    tab: Tab,
    item: Option<String>,
    question: String,
    error: Option<String>,
) -> AiPanel {
    let cfg = &state.config.dashboard.ai;
    let tools = super::ai_tools::enabled(mode.ro, cfg.tools_core);
    let mut p = AiPanel {
        configured: cfg.provider.is_some(),
        base: mode.base,
        tab: tab.key(),
        tab_label: tab.label(),
        item: item.clone().unwrap_or_default(),
        item_title: match item.as_deref() {
            Some(i) => item_title(state, tab, i).await,
            None => String::new(),
        },
        provider: provider_label(cfg),
        model: cfg.model.clone(),
        endpoint: ai::endpoint(cfg),
        prepared: String::new(),
        headers: ai::header_lines(cfg),
        preview: String::new(),
        chars: 0,
        tokens: 0,
        truncated: false,
        suggestions: tab.suggestions(item.is_some()),
        question,
        left: ai::budget(cfg).left(ai::today()),
        error,
        tools: tools.iter().map(|t| (t.name, t.title)).collect(),
        max_steps: ai::MAX_STEPS,
    };
    if !p.configured {
        return p;
    }
    let context = match http::quietly(ai_context::build(state, mode.ro, tab, item.as_deref())).await
    {
        Ok(c) => c,
        Err(e) => {
            p.error = Some(e);
            return p;
        }
    };
    let prepared = ai::store(tab, item, context);
    let body = first_body(state, mode, &prepared, QUESTION_SLOT);
    p.preview = serde_json::to_string_pretty(&body).unwrap_or_default();
    p.chars = p.preview.chars().count();
    // WHY: an estimate (4 characters a token) for the preview; the provider's count is shown
    // after the answer.
    p.tokens = p.chars / 4;
    p.truncated = prepared.truncated;
    p.prepared = prepared.id;
    p
}

/// `GET {base}/ai?tab=…&item=…&q=…`: the assistant on a page of its own (also the no-JS path, and
/// the target of every "Ask about this" and "Explain more with AI" link).
async fn ai_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<AiQuery>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) {
        return Err(AppError::NotFound);
    }
    let (tab, item) = resolve(&q, Some("overview"))?;
    if mode.ro && !tab.in_read_only() {
        return Err(AppError::NotFound);
    }
    let question: String =
        q.q.unwrap_or_default()
            .chars()
            .take(ai::MAX_QUESTION_CHARS)
            .collect();
    let p = panel_for(&state, mode, tab, item, question, None).await;
    Ok(ui::page(AiPage {
        chrome: chrome(&state, "Ask AI", "dashboard", flash, theme),
        d: dash(mode, "/ai", None, 0),
        p,
        r: None,
    }))
}

/// `GET {base}/ai/panel?tab=…` (or `?page=<path>`): the panel as a fragment, loaded when "Ask AI"
/// opens.
async fn ai_panel(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Query(q): Query<AiQuery>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) {
        return Err(AppError::NotFound);
    }
    let (tab, item) = resolve(&q, None)?;
    if mode.ro && !tab.in_read_only() {
        return Err(AppError::NotFound);
    }
    let p = panel_for(&state, mode, tab, item, String::new(), None).await;
    Ok(ui::page(AiPanelFragment { p }))
}

#[derive(Debug, Default, Deserialize)]
pub struct AskForm {
    #[serde(default)]
    prepared: String,
    #[serde(default)]
    tab: String,
    #[serde(default)]
    item: String,
    #[serde(default)]
    question: String,
    /// `ask` (default) or `health`.
    #[serde(default)]
    kind: String,
}

/// The browser's run-owner cookie, and a `Set-Cookie` when it has none yet.
// SECURITY: random 128 bits, HttpOnly, SameSite=Strict: a run's stream is bound to the browser
// that started it (test: a_run_stream_is_bound_to_its_browser)
fn owner(headers: &HeaderMap, secure: bool) -> (String, Option<HeaderValue>) {
    if let Some(v) = cookie::get(headers, ai_agent::OWNER_COOKIE).filter(|v| ai_agent::valid_id(v))
    {
        return (v, None);
    }
    let v = ai::random_id();
    let set = format!(
        "{}={v}; Path=/; HttpOnly; SameSite=Strict{}",
        ai_agent::OWNER_COOKIE,
        if secure { "; Secure" } else { "" }
    );
    (v, HeaderValue::from_str(&set).ok())
}

/// Checks the form and starts a run. `Err`: why nothing was sent.
fn start_run(
    state: &AppState,
    mode: Mode,
    f: &AskForm,
    owner_cookie: String,
) -> Result<std::sync::Arc<ai_agent::Run>, String> {
    let fail = |m: String| m;
    let cfg = &state.config.dashboard.ai;
    if cfg.provider.is_none() {
        return Err(fail(ai::AiError::NotConfigured.message()));
    }
    let (tab, item) = tab_and_item(Some(&f.tab), Some(&f.item))
        .map_err(|_| fail("That page isn't one the assistant knows.".into()))?;
    if mode.ro && !tab.in_read_only() {
        return Err(fail("Not available in the read-only view.".into()));
    }
    let kind = if f.kind == "health" {
        Kind::Health
    } else {
        Kind::Ask
    };
    let question: String = match kind {
        Kind::Health => ai::HEALTH_QUESTION.to_owned(),
        _ => f
            .question
            .trim()
            .chars()
            .take(ai::MAX_QUESTION_CHARS)
            .collect(),
    };
    if question.is_empty() {
        return Err(fail(
            "Type a question first (or pick a suggested one).".into(),
        ));
    }
    // SECURITY: only a context the developer was shown goes out: the prepared id must exist and
    // match the page; otherwise nothing is sent (test: only_the_previewed_context_is_sent)
    let Some(p) = ai::prepared(&f.prepared).filter(|p| p.tab == tab && p.item == item) else {
        return Err(fail(
            "The preview expired or belongs to another page. Review the refreshed preview, then ask again.".into(),
        ));
    };
    let spec = ai_agent::Spec {
        tab,
        item,
        kind,
        first: ai::user_message(&p, &question),
        system: p.system.clone(),
        question,
        owner_cookie: Some(owner_cookie),
        ro: mode.ro,
        base: mode.base,
        reserved: false,
    };
    ai_agent::start(state, spec).map_err(|e| fail(e.message()))
}

fn with_cookie(mut res: Response, set: Option<HeaderValue>) -> Response {
    if let Some(v) = set {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    res
}

/// `POST {base}/ai/runs`: starts a run. With the script (`x-dashboard-fragment`) it answers the
/// run's live fragment, which opens the stream; without, it redirects to the run's page.
async fn ai_runs(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    headers: HeaderMap,
    Form(f): Form<AskForm>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) {
        return Err(AppError::NotFound);
    }
    let (cookie_value, set) = owner(&headers, state.config.is_production());
    let fragment = headers.contains_key("x-dashboard-fragment");
    let res = match start_run(&state, mode, &f, cookie_value) {
        Ok(run) => {
            if fragment {
                ui::page(RunFragment {
                    r: run_view(&run.record(), true),
                })
            } else {
                Redirect::to(&format!("{}/ai/runs/{}?wait=1", mode.base, run.id)).into_response()
            }
        }
        Err(m) => ui::page(RunFragment {
            r: RunView::error(mode.base, m),
        }),
    };
    Ok(with_cookie(res, set))
}

/// `POST {base}/ai/ask`: starts a run and waits for it (up to `NO_JS_WAIT`), then answers the run
/// (finished, or as far as it got, with a link that waits again): a fragment with
/// `x-dashboard-fragment`, else the whole page.
async fn ai_ask(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    headers: HeaderMap,
    theme: ThemeSwitch,
    Form(f): Form<AskForm>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) {
        return Err(AppError::NotFound);
    }
    let (tab, item) = tab_and_item(Some(&f.tab), Some(&f.item))?;
    if mode.ro && !tab.in_read_only() {
        return Err(AppError::NotFound);
    }
    let (cookie_value, set) = owner(&headers, state.config.is_production());
    let view = match start_run(&state, mode, &f, cookie_value) {
        Ok(run) => {
            ai_agent::wait(&run, NO_JS_WAIT).await;
            let rec = run.record();
            run_view(&rec, !rec.status.finished())
        }
        Err(m) => RunView::error(mode.base, m),
    };
    if headers.contains_key("x-dashboard-fragment") {
        return Ok(with_cookie(ui::page(RunFragment { r: view }), set));
    }
    let p = panel_for(&state, mode, tab, item, f.question.clone(), None).await;
    Ok(with_cookie(
        ui::page(AiPage {
            chrome: chrome(&state, "Ask AI", "dashboard", None, theme),
            d: dash(mode, "/ai", None, 0),
            p,
            r: Some(view),
        }),
        set,
    ))
}

/// A live run, if this browser owns it (the watcher's runs belong to every dashboard viewer).
fn owned_run(headers: &HeaderMap, id: &str) -> Option<std::sync::Arc<ai_agent::Run>> {
    let run = ai_agent::get(id)?;
    let cookie = cookie::get(headers, ai_agent::OWNER_COOKIE);
    run.owned_by(cookie.as_deref()).then_some(run)
}

/// `GET {base}/ai/runs/{id}`: a run's page: live (its browser only) with the stream, or finished
/// (from memory or the history). `?wait=1` (the no-JS redirect) waits for the end first.
async fn run_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Path(id): Path<String>,
    Query(q): Query<AiQuery>,
    headers: HeaderMap,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) || !ai_agent::valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let rec = if let Some(run) = ai_agent::get(&id) {
        let owns = run.owned_by(cookie::get(&headers, ai_agent::OWNER_COOKIE).as_deref());
        // SECURITY: a live run is its browser's only; once finished it is in the history every
        // dashboard viewer sees, so its permalink opens in any browser, as a run read back from
        // disk does (test: a_run_stream_is_bound_to_its_browser)
        if !owns && !run.record().status.finished() {
            return Err(AppError::NotFound);
        }
        if owns && q.wait.is_some() {
            ai_agent::wait(&run, NO_JS_WAIT).await;
        }
        run.record()
    } else {
        ai_agent::find_record(&id).ok_or(AppError::NotFound)?
    };
    if rec.base != mode.base {
        return Err(AppError::NotFound);
    }
    let live = !rec.status.finished();
    Ok(ui::page(RunPage {
        chrome: chrome(&state, "Investigation", "dashboard", flash, theme),
        d: dash(mode, "/ai", None, 0),
        r: run_view(&rec, live),
    }))
}

/// `GET {base}/ai/runs/{id}/stream`: each step as it happens (`step`, with its number as the
/// event id, so a reconnect resumes), then the end (`end`: the answer, actions, fix prompt).
async fn run_stream(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Path(id): Path<String>,
    Query(q): Query<AiQuery>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    if !ai::allowed(&state.config, mode.ro) || !ai_agent::valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let run = owned_run(&headers, &id).ok_or(AppError::NotFound)?;
    let resume: usize = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .or(q.from)
        .unwrap_or(0);
    let rx = run.subscribe();
    let s = futures::stream::unfold(
        (run, rx, resume, false),
        |(run, mut rx, sent, done)| async move {
            if done {
                return None;
            }
            loop {
                let rec = run.record();
                if rec.steps.len() > sent {
                    let html = StepFragment {
                        s: step_view(&rec.steps[sent], &rec.base),
                    }
                    .render()
                    .unwrap_or_default();
                    let ev = Event::default()
                        .event("step")
                        .id((sent + 1).to_string())
                        .data(html);
                    return Some((Ok(ev), (run, rx, sent + 1, false)));
                }
                if rec.status.finished() {
                    let html = EndFragment {
                        r: run_view(&rec, false),
                    }
                    .render()
                    .unwrap_or_default();
                    let ev = Event::default().event("end").data(html);
                    return Some((Ok(ev), (run, rx, sent, true)));
                }
                if tokio::time::timeout(Duration::from_secs(20), rx.changed())
                    .await
                    .is_ok_and(|r| r.is_err())
                {
                    // The run is gone from memory; its record is final.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        },
    );
    Ok(Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// `POST {base}/ai/runs/{id}/continue`: one more call for a cut answer, in the same run (its
/// browser only, like the stream). With the script it answers the run's live fragment; without,
/// it redirects to the run's page, which waits for the end.
async fn run_continue(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) || !ai_agent::valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let run = owned_run(&headers, &id).ok_or(AppError::NotFound)?;
    if run.record().base != mode.base {
        return Err(AppError::NotFound);
    }
    let view = match ai_agent::continue_run(&state, &run) {
        Ok(()) => run_view(&run.record(), true),
        Err(e) => {
            let mut v = run_view(&run.record(), false);
            v.error = Some(e.message());
            v
        }
    };
    if headers.contains_key("x-dashboard-fragment") {
        return Ok(ui::page(RunFragment { r: view }));
    }
    Ok(Redirect::to(&format!("{}/ai/runs/{id}?wait=1", mode.base)).into_response())
}

/// `GET {base}/ai/history`: past runs, newest first (live ones included).
async fn history_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Result<Response, AppError> {
    if !ai::allowed(&state.config, mode.ro) {
        return Err(AppError::NotFound);
    }
    let now = chrono::Utc::now();
    let rows = ai_agent::history()
        .into_iter()
        .filter(|r| r.base == mode.base)
        .map(|r| HistoryRow {
            kind: r.kind.label(),
            tab: r.tab.clone(),
            question: if r.kind == Kind::Health {
                "How healthy is this app right now?".to_owned()
            } else {
                ui::truncate(&r.question, 160)
            },
            status: r.status.key(),
            status_text: match &r.status {
                Status::Stopped(w) | Status::Failed(w) => ui::truncate(w, 80),
                Status::Done => "answered".into(),
                Status::Running => "running".into(),
            },
            when: ui::ago(r.started, now),
            calls: r.calls,
            tokens: ui::thousands(r.input_tokens + r.output_tokens),
            id: r.id,
        })
        .collect();
    Ok(ui::page(HistoryPage {
        chrome: chrome(&state, "Ask AI history", "dashboard", flash, theme),
        d: dash(mode, "/ai", None, 0),
        rows,
        keep: ai_agent::MAX_HISTORY,
    }))
}

/// `GET {base}/explain`: "What am I looking at?", static, no key needed.
pub async fn explain_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let groups = ai_explain::GROUPS
        .iter()
        .map(|g| {
            (
                *g,
                ai_explain::PARTS.iter().filter(|p| p.group == *g).collect(),
            )
        })
        .collect();
    ui::page(ExplainPage {
        chrome: chrome(&state, "What am I looking at?", "dashboard", flash, theme),
        d: dash(mode, "/explain", None, 0).ai(&state.config, Tab::Overview, None),
        groups,
    })
}

// ---- the overview's incidents and digest (the command center is ai_home.rs) ------------------------------------------------

pub struct IncidentView {
    pub id: String,
    pub when: String,
    pub kind: &'static str,
    pub severity: &'static str,
    pub title: String,
    pub evidence: Vec<ai::Block>,
    pub href: String,
    pub note: String,
    /// The investigation: its page, status, and (when done) its answer.
    pub run_page: String,
    pub run_status: String,
    pub narrative: Vec<ai::Block>,
}

pub fn incident_views(limit: usize) -> Vec<IncidentView> {
    let now = chrono::Utc::now();
    ai_watch::incidents()
        .into_iter()
        .take(limit)
        .map(|i| {
            let rec = i.run.as_deref().and_then(ai_agent::find_record);
            IncidentView {
                id: i.id.clone(),
                when: ui::ago(i.at, now),
                kind: i.signal.kind,
                severity: i.signal.severity,
                title: i.signal.title.clone(),
                evidence: ai::render_in(&i.signal.evidence, "/dev/dashboard", now.timestamp()),
                href: i.signal.href.clone(),
                note: i.note.clone(),
                run_page: i
                    .run
                    .as_deref()
                    .map(|id| format!("/dev/dashboard/ai/runs/{id}"))
                    .unwrap_or_default(),
                run_status: rec.as_ref().map_or(String::new(), |r| match &r.status {
                    Status::Running => "The agent is investigating…".into(),
                    Status::Done => format!("Investigated in {} calls", r.calls),
                    Status::Stopped(w) => format!("Stopped: {w}"),
                    Status::Failed(w) => format!("Failed: {w}"),
                }),
                narrative: rec
                    .as_ref()
                    .map(|r| answer_blocks(&r.answer, "/dev/dashboard"))
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// The digest's "last seen" from the overview cookie, and the cookie for now.
pub fn seen(headers: &HeaderMap, secure: bool) -> (Option<i64>, HeaderValue) {
    let last = cookie::get(headers, ai_watch::SEEN_COOKIE).and_then(|v| v.parse::<i64>().ok());
    let now = chrono::Utc::now().timestamp();
    (
        last,
        cookie::set(
            ai_watch::SEEN_COOKIE,
            &now.to_string(),
            Some(86_400),
            secure,
        ),
    )
}

/// "1.2s" etc., for templates that format a step's time.
pub fn ms(n: &u64) -> String {
    micros(n * 1_000)
}
