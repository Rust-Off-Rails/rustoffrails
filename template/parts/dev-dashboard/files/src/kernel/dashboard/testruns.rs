//! Test runs: JUnit XML from `cargo nextest run --profile dashboard` (`.config/nextest.toml`),
//! one folder per run in `storage/test-runs/` (git-ignored), on SQLite and on Postgres side by
//! side; slowest tests, failures with their output, flaky tests, llvm-cov's line coverage, and
//! which acceptance criteria in `product/` a test cites (offrails-check's `ac` rule).
//!
//! `app test` and the dashboard's "Run tests" button both run nextest here and stream its progress.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::{DateTime, Utc};
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{HUB, lock};

/// Where runs are kept. APP: another folder (it must be git-ignored).
pub const DIR: &str = "storage/test-runs";
/// Runs kept; older folders are deleted.
pub const KEEP: usize = 30;
/// The nextest profile with JUnit output and one retry (`.config/nextest.toml`).
pub const PROFILE: &str = "dashboard";
/// A JUnit file larger than this isn't read.
const MAX_XML: u64 = 8 * 1024 * 1024;
/// Output kept per failure.
const MAX_OUTPUT: usize = 8_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Db {
    Sqlite,
    Postgres,
}

impl Db {
    pub fn key(self) -> &'static str {
        match self {
            Db::Sqlite => "sqlite",
            Db::Postgres => "postgres",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Db::Sqlite => "SQLite",
            Db::Postgres => "Postgres",
        }
    }
}

/// One suite (one database) of a run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SuiteSummary {
    pub db: Option<Db>,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub flaky: u32,
    pub duration_ms: u64,
}

impl SuiteSummary {
    pub fn total(&self) -> u32 {
        self.passed + self.failed + self.skipped
    }
}

/// `summary.json` in a run's folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub at: DateTime<Utc>,
    pub source: String,
    pub suites: Vec<SuiteSummary>,
}

impl RunSummary {
    pub fn suite(&self, db: Db) -> Option<&SuiteSummary> {
        self.suites.iter().find(|s| s.db == Some(db))
    }

    pub fn failed(&self) -> u32 {
        self.suites.iter().map(|s| s.failed).sum()
    }

    pub fn duration_ms(&self) -> u64 {
        self.suites.iter().map(|s| s.duration_ms).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    /// Failed, then passed on a retry.
    Flaky,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    pub classname: String,
    pub time_ms: u64,
    pub outcome: Outcome,
    pub output: String,
}

/// The test cases of a nextest JUnit report.
pub fn parse_junit(xml: &str) -> Result<Vec<Case>, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut cases = Vec::new();
    let mut current: Option<Case> = None;
    // Inside an element whose text is output: failure, error, system-out, system-err…
    let mut capturing = false;
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        let (element, empty) = match &event {
            Event::Start(e) => (Some(e.clone()), false),
            Event::Empty(e) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = element {
            let name = e.name().as_ref().to_owned();
            if name == "testcase" {
                let mut c = Case {
                    name: String::new(),
                    classname: String::new(),
                    time_ms: 0,
                    outcome: Outcome::Passed,
                    output: String::new(),
                };
                for a in e.attributes().flatten() {
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_default();
                    match a.key.as_ref() {
                        "name" => c.name = v,
                        "classname" => c.classname = v,
                        "time" => c.time_ms = (v.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64,
                        _ => {}
                    }
                }
                if empty {
                    cases.push(c);
                } else {
                    current = Some(c);
                }
            } else if let Some(c) = current.as_mut() {
                match name.as_str() {
                    "failure" | "error" => c.outcome = Outcome::Failed,
                    "skipped" => c.outcome = Outcome::Skipped,
                    "flakyFailure" | "flakyError" if c.outcome == Outcome::Passed => {
                        c.outcome = Outcome::Flaky;
                    }
                    _ => {}
                }
                capturing = !empty
                    && matches!(
                        name.as_str(),
                        "failure" | "error" | "system-out" | "system-err" | "rerunFailure"
                    );
                if let Some(msg) = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == "message")
                    .and_then(|a| {
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .ok()
                            .map(|v| v.into_owned())
                    })
                {
                    push_output(&mut c.output, &msg);
                    push_output(&mut c.output, "\n");
                }
            }
            continue;
        }
        match event {
            Event::Text(t) if capturing => {
                if let Some(c) = current.as_mut() {
                    push_output(&mut c.output, &t.xml10_content());
                }
            }
            Event::CData(t) if capturing => {
                if let Some(c) = current.as_mut() {
                    push_output(&mut c.output, &t.xml10_content());
                }
            }
            Event::GeneralRef(r) if capturing => {
                if let Some(c) = current.as_mut() {
                    let ch = match r.resolve_char_ref() {
                        Ok(Some(ch)) => Some(ch),
                        _ => match &*r {
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "amp" => Some('&'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        },
                    };
                    if let Some(ch) = ch {
                        push_output(&mut c.output, ch.encode_utf8(&mut [0; 4]));
                    }
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == "testcase"
                    && let Some(c) = current.take()
                {
                    cases.push(c);
                }
                capturing = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cases)
}

fn push_output(out: &mut String, s: &str) {
    if out.len() < MAX_OUTPUT {
        let room = MAX_OUTPUT - out.len();
        let mut end = s.len().min(room);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&s[..end]);
    }
}

/// The runs in `dir`, newest first.
pub fn load_runs(dir: &Path) -> Vec<RunSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut runs: Vec<RunSummary> = entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("summary.json")).ok())
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    runs.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
    runs.truncate(KEEP);
    runs
}

/// A run's cases on one database.
pub fn load_cases(dir: &Path, run: &str, db: Db) -> Vec<Case> {
    if !safe_id(run) {
        return Vec::new();
    }
    let path = dir.join(run).join(format!("{}.xml", db.key()));
    match std::fs::metadata(&path) {
        Ok(m) if m.len() <= MAX_XML => {}
        _ => return Vec::new(),
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|xml| parse_junit(&xml).ok())
        .unwrap_or_default()
}

/// Run ids are timestamps (`20261016-153000`); anything else never becomes a path.
// SECURITY: the run id comes from the URL; only [0-9a-z-] reaches the filesystem
// (test: run_ids_never_leave_the_runs_folder).
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == '-')
}

/// Tests whose outcome flipped between passed and failed across `history` (oldest first), plus
/// those that passed on a retry in any run.
pub fn flaky(history: &[Vec<Case>]) -> Vec<(String, u32)> {
    let mut flips: BTreeMap<String, (Option<bool>, u32)> = BTreeMap::new();
    for cases in history {
        for c in cases {
            let key = format!("{} {}", c.classname, c.name);
            let e = flips.entry(key).or_insert((None, 0));
            if c.outcome == Outcome::Flaky {
                e.1 += 1;
            }
            let passed = match c.outcome {
                Outcome::Passed | Outcome::Flaky => Some(true),
                Outcome::Failed => Some(false),
                Outcome::Skipped => None,
            };
            if let (Some(prev), Some(now)) = (e.0, passed)
                && prev != now
            {
                e.1 += 1;
            }
            if passed.is_some() {
                e.0 = passed;
            }
        }
    }
    let mut out: Vec<(String, u32)> = flips
        .into_iter()
        .filter(|(_, (_, n))| *n > 0)
        .map(|(k, (_, n))| (k, n))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Line coverage from `cargo llvm-cov nextest --json --summary-only --output-path
/// storage/test-runs/coverage.json`, if present.
pub fn coverage(dir: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(dir.join("coverage.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.pointer("/data/0/totals/lines/percent")?.as_f64()
}

/// Acceptance criteria in `product/` and whether a test cites them.
#[derive(Default)]
pub struct SpecCoverage {
    pub total: usize,
    pub cited: usize,
    pub uncited: Vec<(String, String)>,
}

/// offrails-check's `ac` rule: every `- [ ] AC-<feature>-<n>` (or `- AC-…`) definition line in
/// `product/` (struck-through ones dropped) should be cited somewhere under `tests/` or `src/`.
pub fn spec_coverage(root: &Path) -> SpecCoverage {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for file in files(&root.join("product"), "md") {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) else {
                continue;
            };
            let rest = rest
                .trim_start_matches("[ ] ")
                .trim_start_matches("[x] ")
                .trim_start_matches("[X] ");
            if !rest.starts_with("AC-") {
                continue;
            }
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if id
                .rsplit('-')
                .next()
                .is_some_and(|n| n.parse::<u32>().is_ok())
            {
                let shown = file
                    .strip_prefix(root)
                    .unwrap_or(&file)
                    .display()
                    .to_string();
                ids.entry(id).or_insert(shown);
            }
        }
    }
    let mut sources = String::new();
    for dir in ["tests", "src"] {
        for file in files(&root.join(dir), "rs") {
            if let Ok(text) = std::fs::read_to_string(file) {
                sources.push_str(&text);
                sources.push('\n');
            }
        }
    }
    let mut out = SpecCoverage {
        total: ids.len(),
        ..SpecCoverage::default()
    };
    for (id, file) in ids {
        let cited = sources.match_indices(&id).any(|(at, _)| {
            !sources[at + id.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        });
        if cited {
            out.cited += 1;
        } else {
            out.uncited.push((id, file));
        }
    }
    out
}

/// Files with extension `ext` under `dir` (at most 2,000; symlinks not followed).
fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().is_some_and(|x| x == ext) {
                out.push(p);
                if out.len() >= 2_000 {
                    return out;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Running nextest
// ---------------------------------------------------------------------------------------------

/// What a run in progress shows.
#[derive(Clone, Default)]
pub struct Progress {
    pub running: bool,
    pub db: Option<Db>,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub current: String,
    pub lines: VecDeque<String>,
    pub started: Option<DateTime<Utc>>,
    pub finished_run: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct RunnerState {
    running: AtomicBool,
    pub progress: Mutex<Progress>,
    /// Tests replace the command (`["sh", "-c", "…"]`) so the suite never runs itself.
    pub command: Mutex<Option<Vec<String>>>,
    /// Tests point the page and the button at a temporary app root.
    pub root: Mutex<Option<PathBuf>>,
}

/// The app root the Tests page reads and the button runs in (`.`, the server's directory).
pub fn root() -> PathBuf {
    lock(&HUB.tests.root)
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
}

impl RunnerState {
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// What nextest printed on one stderr line, for the progress bar.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Starting(u32),
    Passed(String),
    Failed(String),
    Skipped(String),
    Other,
}

/// `Starting 93 tests across 3 binaries`, `PASS [   0.012s] (1/93) crate::bin test_name`.
pub fn parse_line(line: &str) -> Line {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix("Starting ") {
        return rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .map_or(Line::Other, Line::Starting);
    }
    let mut words = t.split_whitespace();
    let status = words.next().unwrap_or_default();
    let name = t
        .rsplit_once(") ")
        .map(|(_, n)| n.to_owned())
        .or_else(|| t.rsplit_once("] ").map(|(_, n)| n.to_owned()))
        .unwrap_or_default();
    match status {
        "PASS" | "FLAKY" => Line::Passed(name),
        "FAIL" | "SIGSEGV" | "SIGABRT" | "TIMEOUT" | "LEAK-FAIL" => Line::Failed(name),
        "SKIP" => Line::Skipped(name),
        _ => Line::Other,
    }
}

fn now_id() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Runs nextest on each database in turn and writes the run folder. `on_line` sees every line.
/// `pg_url` is the admin URL the Postgres suite creates its databases in.
pub async fn run(
    root: &Path,
    target_dir: &Path,
    dbs: &[Db],
    pg_url: Option<&str>,
    source: &str,
    on_line: impl Fn(&str),
) -> Result<RunSummary, String> {
    if HUB.tests.running.swap(true, Ordering::SeqCst) {
        return Err("Tests are already running.".into());
    }
    let result = run_inner(root, target_dir, dbs, pg_url, source, &on_line).await;
    {
        let mut p = lock(&HUB.tests.progress);
        p.running = false;
        match &result {
            Ok(r) => p.finished_run = Some(r.id.clone()),
            Err(e) => p.error = Some(e.clone()),
        }
    }
    HUB.tests.running.store(false, Ordering::SeqCst);
    result
}

async fn run_inner(
    root: &Path,
    target_dir: &Path,
    dbs: &[Db],
    pg_url: Option<&str>,
    source: &str,
    on_line: &impl Fn(&str),
) -> Result<RunSummary, String> {
    let id = now_id();
    let folder = root.join(DIR).join(&id);
    std::fs::create_dir_all(&folder).map_err(|e| format!("creating {DIR}: {e}"))?;
    *lock(&HUB.tests.progress) = Progress {
        running: true,
        started: Some(Utc::now()),
        ..Progress::default()
    };
    let mut suites = Vec::new();
    for db in dbs {
        {
            let mut p = lock(&HUB.tests.progress);
            p.db = Some(*db);
            p.total = 0;
            p.passed = 0;
            p.failed = 0;
            p.skipped = 0;
        }
        let started = Instant::now();
        let command = lock(&HUB.tests.command).clone().unwrap_or_else(|| {
            [
                "cargo",
                "nextest",
                "run",
                "--profile",
                PROFILE,
                "--no-fail-fast",
                "--color",
                "never",
            ]
            .map(str::to_owned)
            .to_vec()
        });
        let (program, args) = command.split_first().ok_or("empty test command")?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // WHY: the SQLite suite must not see a Postgres URL from .env, and the Postgres suite gets
        // exactly the configured one.
        match (db, pg_url) {
            (Db::Postgres, Some(url)) => {
                cmd.env("TEST_DATABASE_URL", url);
            }
            (Db::Postgres, None) => {
                return Err("TEST_DATABASE_URL isn't set: no Postgres to run on.".into());
            }
            (Db::Sqlite, _) => {
                cmd.env_remove("TEST_DATABASE_URL");
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting {program}: {e}"))?;
        let out = child.stdout.take();
        let err = child.stderr.take();
        let mut lines_out = out.map(|o| BufReader::new(o).lines());
        let mut lines_err = err.map(|e| BufReader::new(e).lines());
        loop {
            let line = tokio::select! {
                l = async { match lines_err.as_mut() { Some(r) => r.next_line().await, None => Ok(None) } } => l,
                l = async { match lines_out.as_mut() { Some(r) => r.next_line().await, None => Ok(None) } } => l,
            };
            match line {
                Ok(Some(line)) => {
                    on_line(&line);
                    progress_line(&line);
                }
                _ => {
                    // One stream ended: drain both.
                    if let Some(r) = lines_err.as_mut() {
                        while let Ok(Some(line)) = r.next_line().await {
                            on_line(&line);
                            progress_line(&line);
                        }
                    }
                    if let Some(r) = lines_out.as_mut() {
                        while let Ok(Some(line)) = r.next_line().await {
                            on_line(&line);
                            progress_line(&line);
                        }
                    }
                    break;
                }
            }
        }
        let _ = child.wait().await;
        let junit = target_dir.join("nextest").join(PROFILE).join("junit.xml");
        let dest = folder.join(format!("{}.xml", db.key()));
        let mut summary = SuiteSummary {
            db: Some(*db),
            duration_ms: started.elapsed().as_millis() as u64,
            ..SuiteSummary::default()
        };
        if std::fs::copy(&junit, &dest).is_ok() {
            for c in load_cases(&root.join(DIR), &id, *db) {
                match c.outcome {
                    Outcome::Passed => summary.passed += 1,
                    Outcome::Failed => summary.failed += 1,
                    Outcome::Skipped => summary.skipped += 1,
                    Outcome::Flaky => {
                        summary.passed += 1;
                        summary.flaky += 1;
                    }
                }
            }
        } else {
            let p = lock(&HUB.tests.progress);
            summary.passed = p.passed;
            summary.failed = p.failed;
            summary.skipped = p.skipped;
        }
        suites.push(summary);
    }
    let run = RunSummary {
        id,
        at: Utc::now(),
        source: source.to_owned(),
        suites,
    };
    write_summary(&root.join(DIR), &run)?;
    Ok(run)
}

fn progress_line(line: &str) {
    let mut p = lock(&HUB.tests.progress);
    match parse_line(line) {
        Line::Starting(n) => p.total = n,
        Line::Passed(n) => {
            p.passed += 1;
            p.current = n;
        }
        Line::Failed(n) => {
            p.failed += 1;
            p.current = n.clone();
            p.lines.push_back(format!("FAIL {n}"));
        }
        Line::Skipped(_) => p.skipped += 1,
        Line::Other => {}
    }
    while p.lines.len() > 20 {
        p.lines.pop_front();
    }
}

/// Writes `summary.json` and prunes runs beyond `KEEP`.
pub fn write_summary(dir: &Path, run: &RunSummary) -> Result<(), String> {
    let folder = dir.join(&run.id);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(run).map_err(|e| e.to_string())?;
    std::fs::write(folder.join("summary.json"), json).map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| safe_id(n))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    while ids.len() > KEEP {
        let old = ids.remove(0);
        let _ = std::fs::remove_dir_all(dir.join(old));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextest_progress_lines_parse() {
        assert_eq!(
            parse_line("    Starting 93 tests across 3 binaries"),
            Line::Starting(93)
        );
        assert_eq!(
            parse_line("        PASS [   0.012s] (1/93) offrails_host::app smoke_test::home"),
            Line::Passed("offrails_host::app smoke_test::home".into())
        );
        assert!(matches!(
            parse_line("        FAIL [   1.2s] (2/93) x::y z"),
            Line::Failed(_)
        ));
        assert_eq!(parse_line("   Compiling foo"), Line::Other);
    }

    #[test]
    fn run_ids_never_leave_the_runs_folder() {
        assert!(safe_id("20261016-153000"));
        assert!(!safe_id("../../etc"));
        assert!(!safe_id("a/b"));
        assert!(!safe_id(""));
        assert!(load_cases(Path::new("/"), "../etc", Db::Sqlite).is_empty());
    }
}
