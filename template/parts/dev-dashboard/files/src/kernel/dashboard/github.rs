//! CI and repository status for the app's own GitHub repo, read-only: the latest workflow runs and
//! their success rate, open pull requests and issues, the latest release, and open Dependabot
//! alerts when the token may read them.
//!
//! `GITHUB_REPO=owner/name`, `GITHUB_TOKEN` optional (public repos work without one, at 60
//! requests an hour). Network calls happen only when the page is opened, never at boot; each
//! answer is cached for 60 s, and the rate-limit headers are respected.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;

use super::lock;
use crate::config::DashboardConfig;

/// How long an answer is reused.
pub const TTL: Duration = Duration::from_secs(60);
/// Per request.
pub const TIMEOUT: Duration = Duration::from_secs(4);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Workflow runs fetched.
const RUNS: usize = 50;

#[derive(Clone)]
struct Fetched {
    status: u16,
    body: Value,
    at: Instant,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RateLimit {
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// Unix seconds.
    pub reset: Option<i64>,
}

#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<String, Fetched>>,
    rate: Mutex<RateLimit>,
}

impl Cache {
    pub fn clear(&self) {
        lock(&self.entries).clear();
    }

    pub fn rate(&self) -> RateLimit {
        *lock(&self.rate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    RateLimited(i64),
    Timeout,
    Network,
    Status(u16),
}

impl FetchError {
    /// For the page: what went wrong, never a URL or a header.
    pub fn message(&self) -> String {
        match self {
            FetchError::RateLimited(reset) => format!(
                "GitHub's rate limit is used up until {} (set GITHUB_TOKEN for 5,000 an hour)",
                DateTime::from_timestamp(*reset, 0)
                    .map(|t| t.format("%H:%M UTC").to_string())
                    .unwrap_or_else(|| "later".into())
            ),
            FetchError::Timeout => format!("GitHub didn't answer within {} s", TIMEOUT.as_secs()),
            FetchError::Network => "couldn't reach GitHub (offline?)".into(),
            FetchError::Status(401) => "GitHub refused the token (401): check GITHUB_TOKEN".into(),
            FetchError::Status(404) => {
                "GitHub says the repository doesn't exist, or the token can't see it (404)".into()
            }
            FetchError::Status(s) => format!("GitHub answered {s}"),
        }
    }
}

static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .ok()
});

fn headers(cfg: &DashboardConfig) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    h.insert(
        USER_AGENT,
        HeaderValue::from_static("offrails-dev-dashboard"),
    );
    h.insert(
        "x-github-api-version",
        HeaderValue::from_static("2022-11-28"),
    );
    if let Some(token) = &cfg.github_token
        && let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
        // SECURITY: the token goes in this header only; it is marked sensitive (left out of
        // debug output), never logged, never rendered (test: the_github_token_is_never_shown_or_logged).
        v.set_sensitive(true);
        h.insert(AUTHORIZATION, v);
    }
    h
}

/// The fixture file standing in for an API path (`DASHBOARD_GITHUB_FIXTURE`, development only).
fn fixture_name(path: &str) -> &'static str {
    let bare = path.split('?').next().unwrap_or(path);
    if bare.ends_with("/actions/runs") {
        "runs.json"
    } else if bare.ends_with("/pulls") {
        "pulls.json"
    } else if bare.ends_with("/releases/latest") {
        "release.json"
    } else if bare.ends_with("/dependabot/alerts") {
        "alerts.json"
    } else {
        "repo.json"
    }
}

/// GET an API path (`/repos/o/r/...`), from the cache when fresh.
async fn get(cfg: &DashboardConfig, cache: &Cache, path: &str) -> Result<(u16, Value), FetchError> {
    if let Some(f) = lock(&cache.entries).get(path)
        && f.at.elapsed() < TTL
    {
        return Ok((f.status, f.body.clone()));
    }
    if let Some(dir) = &cfg.github_fixture {
        let file = PathBuf::from(dir).join(fixture_name(path));
        return match std::fs::read_to_string(file) {
            Ok(text) => Ok((200, serde_json::from_str(&text).unwrap_or(Value::Null))),
            Err(_) => Ok((404, Value::Null)),
        };
    }
    let rate = cache.rate();
    if rate.remaining == Some(0)
        && let Some(reset) = rate.reset
        && reset > Utc::now().timestamp()
    {
        return stale(cache, path).ok_or(FetchError::RateLimited(reset));
    }
    let client = CLIENT.as_ref().ok_or(FetchError::Network)?;
    let url = format!("{}{path}", cfg.github_api.trim_end_matches('/'));
    let res = client
        .get(url)
        .headers(headers(cfg))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })?;
    let status = res.status().as_u16();
    let num = |name: &str| {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
    };
    let new_rate = RateLimit {
        limit: num("x-ratelimit-limit").map(|v| v as u32),
        remaining: num("x-ratelimit-remaining").map(|v| v as u32),
        reset: num("x-ratelimit-reset"),
    };
    let retry_after = num("retry-after");
    if new_rate.remaining.is_some() {
        *lock(&cache.rate) = new_rate;
    }
    if (status == 403 || status == 429) && (new_rate.remaining == Some(0) || retry_after.is_some())
    {
        let reset = retry_after
            .map(|s| Utc::now().timestamp() + s)
            .or(new_rate.reset)
            .unwrap_or_else(|| Utc::now().timestamp() + 60);
        if retry_after.is_some() {
            *lock(&cache.rate) = RateLimit {
                remaining: Some(0),
                reset: Some(reset),
                ..new_rate
            };
        }
        return stale(cache, path).ok_or(FetchError::RateLimited(reset));
    }
    let body: Value = res.json().await.unwrap_or(Value::Null);
    lock(&cache.entries).insert(
        path.to_owned(),
        Fetched {
            status,
            body: body.clone(),
            at: Instant::now(),
        },
    );
    Ok((status, body))
}

/// The last answer, however old (shown when GitHub can't be asked).
fn stale(cache: &Cache, path: &str) -> Option<(u16, Value)> {
    lock(&cache.entries)
        .get(path)
        .map(|f| (f.status, f.body.clone()))
}

/// Only https links from the API become hrefs.
// SECURITY: a `javascript:` URL in API data never reaches an href (test: github_links_are_https_only).
pub fn safe_href(v: Option<&str>) -> Option<String> {
    v.filter(|u| u.starts_with("https://") && !u.chars().any(char::is_whitespace))
        .map(str::to_owned)
}

pub struct RunView {
    pub name: String,
    pub title: String,
    pub branch: String,
    pub sha: String,
    pub event: String,
    pub status: String,
    /// `ok`, `err`, `warn`, `info` or `muted`.
    pub tone: &'static str,
    pub duration: String,
    pub when: String,
    pub href: Option<String>,
    pub number: u64,
}

pub struct Day {
    pub date: String,
    pub runs: u32,
    pub success: u32,
}

impl Day {
    pub fn percent(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.success) * 100.0 / f64::from(self.runs)
        }
    }
}

pub struct Release {
    pub tag: String,
    pub name: String,
    pub when: String,
    pub href: Option<String>,
}

pub enum Alerts {
    Count(usize),
    NotPermitted,
    Unknown,
}

/// Everything the page shows.
pub struct GithubView {
    pub repo: Option<String>,
    pub html_url: Option<String>,
    pub description: String,
    pub default_branch: String,
    pub stars: u64,
    pub runs: Vec<RunView>,
    pub days: Vec<Day>,
    pub success_rate: Option<f64>,
    pub open_prs: Option<String>,
    pub open_issues: Option<u64>,
    pub release: Option<Release>,
    pub alerts: Alerts,
    pub rate: RateLimit,
    pub errors: Vec<String>,
    pub token: bool,
    pub fixture: bool,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn time(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// Loads the page's data: five API calls at once (or fewer from the cache).
pub async fn load(cfg: &DashboardConfig, cache: &Cache) -> GithubView {
    let mut view = GithubView {
        repo: cfg.github_repo.clone(),
        html_url: None,
        description: String::new(),
        default_branch: String::new(),
        stars: 0,
        runs: Vec::new(),
        days: Vec::new(),
        success_rate: None,
        open_prs: None,
        open_issues: None,
        release: None,
        alerts: Alerts::Unknown,
        rate: cache.rate(),
        errors: Vec::new(),
        token: cfg.github_token.is_some(),
        fixture: cfg.github_fixture.is_some(),
    };
    let Some(repo) = cfg.github_repo.clone() else {
        return view;
    };
    let base = format!("/repos/{repo}");
    let (p_runs, p_pulls, p_release, p_alerts) = (
        format!("{base}/actions/runs?per_page={RUNS}"),
        format!("{base}/pulls?state=open&per_page=100"),
        format!("{base}/releases/latest"),
        format!("{base}/dependabot/alerts?state=open&per_page=100"),
    );
    let (r_repo, r_runs, r_pulls, r_release, r_alerts) = futures::join!(
        get(cfg, cache, &base),
        get(cfg, cache, &p_runs),
        get(cfg, cache, &p_pulls),
        get(cfg, cache, &p_release),
        get(cfg, cache, &p_alerts),
    );
    let now = Utc::now();
    match r_repo {
        Ok((200, v)) => {
            view.html_url = safe_href(v.get("html_url").and_then(Value::as_str));
            view.description = s(&v, "description");
            view.default_branch = s(&v, "default_branch");
            view.stars = v
                .get("stargazers_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            view.open_issues = v.get("open_issues_count").and_then(Value::as_u64);
        }
        Ok((status, _)) => view.errors.push(FetchError::Status(status).message()),
        Err(e) => view.errors.push(e.message()),
    }
    if let Ok((200, v)) = r_runs {
        let runs = v
            .get("workflow_runs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut days: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        let (mut done, mut good) = (0_u32, 0_u32);
        for r in &runs {
            let status = s(r, "status");
            let conclusion = s(r, "conclusion");
            let started = time(r, "run_started_at").or_else(|| time(r, "created_at"));
            let updated = time(r, "updated_at");
            if status == "completed" && conclusion != "skipped" && conclusion != "cancelled" {
                done += 1;
                let ok = conclusion == "success";
                good += u32::from(ok);
                if let Some(t) = started {
                    let d = days.entry(t.format("%Y-%m-%d").to_string()).or_default();
                    d.0 += 1;
                    d.1 += u32::from(ok);
                }
            }
            let (label, tone) = match (status.as_str(), conclusion.as_str()) {
                ("completed", "success") => ("passed".to_owned(), "ok"),
                ("completed", "failure" | "timed_out" | "startup_failure") => {
                    ("failed".to_owned(), "err")
                }
                ("completed", "cancelled" | "skipped") => (conclusion.clone(), "muted"),
                ("completed", other) => (other.to_owned(), "warn"),
                (other, _) => (other.replace('_', " "), "info"),
            };
            view.runs.push(RunView {
                name: s(r, "name"),
                title: crate::kernel::ui::truncate(&s(r, "display_title"), 90),
                branch: s(r, "head_branch"),
                sha: s(r, "head_sha").chars().take(7).collect(),
                event: s(r, "event"),
                status: label,
                tone,
                duration: match (started, updated, status.as_str()) {
                    (Some(a), Some(b), "completed") => {
                        crate::kernel::ui::duration((b - a).num_milliseconds().max(0))
                    }
                    (Some(a), _, _) => format!(
                        "{} so far",
                        crate::kernel::ui::duration((now - a).num_milliseconds().max(0))
                    ),
                    _ => "–".into(),
                },
                when: started
                    .map(|t| crate::kernel::ui::ago(t, now))
                    .unwrap_or_default(),
                href: safe_href(r.get("html_url").and_then(Value::as_str)),
                number: r.get("run_number").and_then(Value::as_u64).unwrap_or(0),
            });
        }
        view.runs.truncate(20);
        view.success_rate = (done > 0).then(|| f64::from(good) * 100.0 / f64::from(done));
        view.days = days
            .into_iter()
            .rev()
            .take(14)
            .map(|(date, (runs, success))| Day {
                date,
                runs,
                success,
            })
            .collect();
        view.days.reverse();
    }
    if let Ok((200, v)) = r_pulls {
        let n = v.as_array().map_or(0, Vec::len);
        view.open_prs = Some(if n >= 100 {
            "100+".into()
        } else {
            n.to_string()
        });
        // GitHub counts pull requests as issues too.
        if let Some(issues) = view.open_issues.as_mut() {
            *issues = issues.saturating_sub(n as u64);
        }
    }
    if let Ok((200, v)) = r_release {
        view.release = Some(Release {
            tag: s(&v, "tag_name"),
            name: s(&v, "name"),
            when: time(&v, "published_at")
                .map(|t| crate::kernel::ui::ago(t, now))
                .unwrap_or_default(),
            href: safe_href(v.get("html_url").and_then(Value::as_str)),
        });
    }
    view.alerts = match r_alerts {
        Ok((200, v)) => Alerts::Count(v.as_array().map_or(0, Vec::len)),
        Ok((401 | 403 | 404, _)) => Alerts::NotPermitted,
        _ => Alerts::Unknown,
    };
    view.rate = cache.rate();
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_links_are_https_only() {
        assert_eq!(
            safe_href(Some("https://github.com/example-org/example-app")).as_deref(),
            Some("https://github.com/example-org/example-app")
        );
        assert_eq!(safe_href(Some("javascript:alert(1)")), None);
        assert_eq!(safe_href(Some("http://example.com")), None);
        assert_eq!(safe_href(None), None);
    }

    #[test]
    fn api_paths_map_to_fixture_files() {
        assert_eq!(fixture_name("/repos/o/r"), "repo.json");
        assert_eq!(
            fixture_name("/repos/o/r/actions/runs?per_page=50"),
            "runs.json"
        );
        assert_eq!(
            fixture_name("/repos/o/r/dependabot/alerts?state=open"),
            "alerts.json"
        );
    }
}
