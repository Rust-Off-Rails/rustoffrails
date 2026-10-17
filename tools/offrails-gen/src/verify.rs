//! `cargo offrails verify [--fast|--full]`: the Definition of Done as one command, run in
//! parallel where the steps are independent (workflow.md#definition-of-done).
//!
//! `--fast` (the default, for every iteration): `cargo fmt --check`, the CSS build (htmx), clippy
//! (its own target dir, so it compiles while the tests build), the test binaries once, then the
//! SQLite and Postgres suites at the same time. `--full` (once, before you say done): also the
//! mechanical gates (offrails-check), coverage (it is the SQLite run, instrumented), cargo-deny,
//! gitleaks, the no-unwrap lint, `routes` against docs/routes.md, the TS checks when there is a
//! `web/`, and the browser smoke when `BASE` names a running app.
//!
//! Each step writes `target/verify/<step>.log`; the summary prints one line per step and the
//! tail of each failure. Exit status: 0 all green, 1 something failed, 2 usage.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::util::{self, Fail, Result};

pub struct VerifyOptions {
    pub full: bool,
    pub postgres: bool,
    pub test_threads: Option<usize>,
}

/// One command of the plan. `after`: steps that must pass first.
struct Step {
    name: &'static str,
    program: String,
    args: Vec<String>,
    env: Vec<(String, Option<String>)>,
    after: Vec<&'static str>,
    /// Reported, never fails the run (a review list, not a gate).
    advisory: bool,
}

impl Step {
    fn new(name: &'static str, program: &str, args: &[&str]) -> Self {
        Step {
            name,
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: Vec::new(),
            after: Vec::new(),
            advisory: false,
        }
    }
    fn after(mut self, deps: &[&'static str]) -> Self {
        self.after.extend_from_slice(deps);
        self
    }
    fn env(mut self, key: &str, value: Option<&str>) -> Self {
        self.env.push((key.to_owned(), value.map(str::to_owned)));
        self
    }
    fn args(mut self, more: &[String]) -> Self {
        self.args.extend_from_slice(more);
        self
    }
}

/// A suite step's name and environment.
type Suite = (&'static str, Vec<(String, Option<String>)>);

enum Outcome {
    Passed,
    Failed(String),
    Skipped(String),
}

struct Done {
    name: &'static str,
    outcome: Outcome,
    took: Duration,
    note: String,
}

pub fn run(app: &Path, opts: &VerifyOptions) -> Result<()> {
    let started = Instant::now();
    if !have_cargo("nextest") {
        return Err(Fail::new(
            "verify runs the suites with cargo-nextest: cargo install --locked cargo-nextest",
        ));
    }
    let logs = app.join("target").join("verify");
    util::ctx(fs::create_dir_all(&logs), "creating", &logs)?;
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let pg = if opts.postgres {
        postgres_url(app)
    } else {
        Err("--no-postgres".to_owned())
    };
    let (steps, skipped) = plan(app, opts, &pg, cpus);

    println!(
        "verify --{} ({}, {cpus} CPUs, Postgres: {})",
        if opts.full { "full" } else { "fast" },
        util::package_name(app).unwrap_or_else(|_| "app".into()),
        match &pg {
            Ok(url) => redact(url),
            Err(why) => format!("skipped: {why}"),
        }
    );
    // The suites' environments, for re-running their failures alone afterwards.
    let suites: Vec<Suite> = steps
        .iter()
        .filter(|s| s.name.starts_with("tests ("))
        .map(|s| (s.name, s.env.clone()))
        .collect();
    let mut done = execute(app, &logs, steps);
    rerun_failures_alone(app, &logs, &suites, &mut done);
    done.extend(skipped);

    // The live lines above showed each step as it finished; add the ones that never ran.
    let mut failed = 0;
    for d in &done {
        match &d.outcome {
            Outcome::Failed(_) => failed += 1,
            Outcome::Skipped(why) if d.took.is_zero() && !why.ends_with(" failed") => {
                println!("  skip  {:<22} {why}", d.name);
            }
            _ => {}
        }
    }
    for d in &done {
        if let Outcome::Failed(tail) = &d.outcome {
            println!(
                "\n== {} failed (full log: {})\n{tail}",
                d.name,
                logs.join(log_name(d.name)).display()
            );
        }
    }
    println!(
        "{} in {:.1}s",
        if failed == 0 {
            "green".to_owned()
        } else {
            format!("{failed} failed")
        },
        started.elapsed().as_secs_f64()
    );
    if opts.full && failed == 0 {
        println!(
            "Plus the Definition of Done table items no command proves (workflow.md#definition-of-done): \
             rollback then migrate, the real-browser smoke of what you changed, decisions logged."
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(Fail::new(format!("verify: {failed} step(s) failed")))
    }
}

/// The steps for this app, and the ones skipped up front (with why).
fn plan(
    app: &Path,
    opts: &VerifyOptions,
    pg: &std::result::Result<String, String>,
    cpus: usize,
) -> (Vec<Step>, Vec<Done>) {
    let mut steps = Vec::new();
    let mut skipped = Vec::new();
    let skip = |name: &'static str, why: String| Done {
        name,
        outcome: Outcome::Skipped(why),
        took: Duration::ZERO,
        note: String::new(),
    };
    // Both suites run at once, and they wait on sockets, timers and the database more than on
    // the CPU, so each gets every core.
    let threads = opts.test_threads.unwrap_or(cpus).max(1).to_string();
    let nextest = |extra: &[&str]| -> Vec<String> {
        let mut a = vec![
            "nextest",
            "run",
            "--cargo-quiet",
            "--no-fail-fast",
            "--status-level",
            "fail",
            "--final-status-level",
            "flaky",
        ];
        a.extend_from_slice(extra);
        a.into_iter().map(str::to_owned).collect()
    };

    steps.push(Step::new("fmt", "cargo", &["fmt", "--all", "--check"]));

    // htmx: theme_test reads the built CSS, so the suites wait for it.
    let mut test_after = vec!["build tests"];
    if let Some(tw) = tailwind(app) {
        steps.push(Step::new(
            "css",
            &tw,
            &["-i", "assets/app.css", "-o", "static/app.css", "--minify"],
        ));
        test_after.push("css");
    } else if app.join("assets").join("app.css").is_file() {
        skipped.push(skip(
            "css",
            "no bin/tailwindcss: run bin/setup-tailwind once".into(),
        ));
    }

    // Its own target dir: check-mode artifacts never share with the build anyway, and a separate
    // lock lets it run while the tests compile.
    steps.push(
        Step::new(
            "clippy",
            "cargo",
            &["clippy", "--all-targets", "--quiet", "--", "-D", "warnings"],
        )
        .env("CARGO_TARGET_DIR", Some(&target_sub(app, "clippy"))),
    );
    steps.push(Step::new(
        "build tests",
        "cargo",
        &["nextest", "run", "--no-run", "--cargo-quiet"],
    ));

    if opts.full && have_cargo("llvm-cov") {
        // The SQLite suite, instrumented: one run proves the tests and the coverage gate.
        steps.push(
            Step::new(
                "tests (SQLite) + coverage",
                "cargo",
                &[
                    "llvm-cov",
                    "nextest",
                    "--summary-only",
                    "--no-fail-fast",
                    "--ignore-filename-regex",
                    r"src/main\.rs$",
                    "--fail-under-lines",
                    "80",
                    "--status-level",
                    "fail",
                    "--final-status-level",
                    "flaky",
                    "--test-threads",
                    &threads,
                ],
            )
            .env("TEST_DATABASE_URL", None)
            .after(&test_after),
        );
    } else {
        if opts.full {
            skipped.push(skip(
                "coverage",
                "cargo-llvm-cov is not installed: cargo install cargo-llvm-cov".into(),
            ));
        }
        steps.push(
            Step::new("tests (SQLite)", "cargo", &[])
                .args(&nextest(&["--test-threads", &threads]))
                .env("TEST_DATABASE_URL", None)
                .after(&test_after),
        );
    }
    match pg {
        Ok(url) => steps.push(
            Step::new("tests (Postgres)", "cargo", &[])
                // The integration tests (tests/): unit tests in src/ never read
                // TEST_DATABASE_URL, so the SQLite run covers them.
                .args(&nextest(&["--test-threads", &threads, "-E", "kind(test)"]))
                .env("TEST_DATABASE_URL", Some(url))
                .after(&test_after),
        ),
        Err(why) => skipped.push(skip("tests (Postgres)", why.clone())),
    }

    if !opts.full {
        return (steps, skipped);
    }

    let check = app.join(".offrails").join("tools").join("offrails-check");
    if check.is_file() && util::have("sh") {
        steps.push(Step::new(
            "offrails-check",
            "sh",
            &[".offrails/tools/offrails-check"],
        ));
    } else {
        skipped.push(skip("offrails-check", "needs sh and .offrails/".into()));
    }
    let mut no_unwrap = Step::new(
        "no unwrap/expect",
        "cargo",
        &[
            "clippy",
            "--lib",
            "--bins",
            "--quiet",
            "--",
            "-W",
            "clippy::unwrap_used",
            "-W",
            "clippy::expect_used",
        ],
    )
    .env("CARGO_TARGET_DIR", Some(&target_sub(app, "clippy")))
    .after(&["clippy"]);
    no_unwrap.advisory = true;
    steps.push(no_unwrap);
    if have_cargo("deny") {
        steps.push(Step::new("cargo deny", "cargo", &["deny", "check"]));
    } else {
        skipped.push(skip(
            "cargo deny",
            "not installed: cargo install --locked cargo-deny".into(),
        ));
    }
    if util::have("gitleaks") {
        steps.push(Step::new(
            "gitleaks",
            "gitleaks",
            &["git", ".", "--redact", "--no-banner"],
        ));
    } else {
        skipped.push(skip(
            "gitleaks",
            "not installed (brew install gitleaks, or CI runs it)".into(),
        ));
    }
    if app.join("docs").join("routes.md").is_file() && util::have("git") {
        steps.push(
            Step::new(
                "routes",
                if cfg!(windows) { "cmd" } else { "sh" },
                &if cfg!(windows) {
                    [
                        "/C",
                        "cargo run -q -- routes > NUL && git diff --exit-code docs/routes.md",
                    ]
                } else {
                    [
                        "-c",
                        "cargo run -q -- routes > /dev/null && git diff --exit-code docs/routes.md",
                    ]
                },
            )
            .after(&["build tests"]),
        );
    }
    if app.join("web").join("package.json").is_file() {
        for (name, args) in [
            (
                "web: biome",
                &["--dir", "web", "exec", "biome", "check", "."][..],
            ),
            ("web: typecheck", &["--dir", "web", "typecheck"][..]),
            ("web: test", &["--dir", "web", "test", "--run"][..]),
            ("web: build", &["--dir", "web", "build"][..]),
            ("web: audit", &["--dir", "web", "audit", "--prod"][..]),
        ] {
            steps.push(Step::new(name, "pnpm", args));
        }
    }
    let smoke = app
        .join(".offrails")
        .join("tools")
        .join("browser-smoke")
        .join("run.sh");
    match std::env::var("BASE") {
        Ok(base) if !base.is_empty() && smoke.is_file() => {
            steps.push(Step::new(
                "browser smoke",
                "bash",
                &[".offrails/tools/browser-smoke/run.sh"],
            ));
            if app
                .join("tests")
                .join("e2e")
                .join("smoke.scenario.mjs")
                .is_file()
            {
                steps.push(
                    Step::new(
                        "browser a11y",
                        "bash",
                        &[".offrails/tools/browser-smoke/run.sh", "a11y"],
                    )
                    .env("SCENARIO", Some("tests/e2e/smoke.scenario.mjs")),
                );
            }
        }
        _ => skipped.push(skip(
            "browser smoke",
            "set BASE=<the URL bin/dev printed> to run it".into(),
        )),
    }
    (steps, skipped)
}

/// Runs the steps, each as soon as everything it waits for has passed.
fn execute(app: &Path, logs: &Path, steps: Vec<Step>) -> Vec<Done> {
    let mut pending: Vec<Step> = steps;
    let mut status: BTreeMap<&'static str, bool> = BTreeMap::new();
    let mut results = Vec::new();
    let (tx, rx) = mpsc::channel::<Done>();
    let mut running = 0;
    loop {
        // Steps whose dependency failed are skipped; ready ones start.
        let mut i = 0;
        while i < pending.len() {
            let blocked = pending[i]
                .after
                .iter()
                .find(|d| status.get(**d) == Some(&false))
                .copied();
            if let Some(dep) = blocked {
                let s = pending.remove(i);
                status.insert(s.name, false);
                println!("  skip  {:<22}         ({dep} failed)", s.name);
                results.push(Done {
                    name: s.name,
                    outcome: Outcome::Skipped(format!("{dep} failed")),
                    took: Duration::ZERO,
                    note: String::new(),
                });
                continue;
            }
            if pending[i]
                .after
                .iter()
                .all(|d| status.get(*d) == Some(&true))
            {
                let s = pending.remove(i);
                let tx = tx.clone();
                let app = app.to_path_buf();
                let log = logs.join(log_name(s.name));
                running += 1;
                std::thread::spawn(move || {
                    let _ = tx.send(run_step(&app, &log, &s));
                });
                continue;
            }
            i += 1;
        }
        if running == 0 {
            break;
        }
        let Ok(d) = rx.recv() else { break };
        running -= 1;
        let ok = !matches!(d.outcome, Outcome::Failed(_));
        status.insert(d.name, ok);
        let (tag, note) = match &d.outcome {
            Outcome::Passed => ("ok  ", d.note.as_str()),
            Outcome::Failed(_) => ("FAIL", d.note.as_str()),
            Outcome::Skipped(w) => ("skip", w.as_str()),
        };
        println!(
            "  {tag}  {:<22} {:>6.1}s  {note}",
            d.name,
            d.took.as_secs_f64()
        );
        let _ = io::stdout().flush();
        results.push(d);
    }
    // Anything left waits on a step that never ran (a typo in `after`): say so.
    for s in pending {
        results.push(Done {
            name: s.name,
            outcome: Outcome::Failed(format!("waits on {:?}, which is not in the plan", s.after)),
            took: Duration::ZERO,
            note: String::new(),
        });
    }
    results
}

fn run_step(app: &Path, log: &Path, s: &Step) -> Done {
    let started = Instant::now();
    let finish = |outcome: Outcome, note: String| Done {
        name: s.name,
        outcome,
        took: started.elapsed(),
        note,
    };
    let Ok(file) = File::create(log) else {
        return finish(
            Outcome::Failed(format!("can't write {}", log.display())),
            String::new(),
        );
    };
    let Ok(err) = file.try_clone() else {
        return finish(
            Outcome::Failed("can't clone the log handle".into()),
            String::new(),
        );
    };
    let mut cmd = Command::new(&s.program);
    cmd.args(&s.args)
        .current_dir(app)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err))
        .env("CARGO_TERM_COLOR", "never")
        .env("NEXTEST_HIDE_PROGRESS_BAR", "1");
    for (k, v) in &s.env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let status = cmd.status();
    let text = fs::read_to_string(log).unwrap_or_default();
    let note = summary_note(&text);
    match status {
        Ok(st) if st.success() => finish(Outcome::Passed, note),
        Ok(_) if s.advisory => finish(
            Outcome::Passed,
            format!("{note} (advisory: review the log)"),
        ),
        Ok(st) => finish(Outcome::Failed(tail(&text, 60)), format!("{note} ({st})")),
        Err(e) => finish(
            Outcome::Failed(format!("can't run {}: {e}", s.program)),
            String::new(),
        ),
    }
}

/// The two suites run at the same time, in the same directory: a test that keeps state on disk
/// (under `storage/`) can trip over its twin in the other suite. Each failed test is run once
/// more, alone, after everything else. One that passes then is reported as **flaky** (still a
/// failure: a test that depends on what else runs is a bug, test-standards.md), so the reader
/// knows it is shared state and not the change.
fn rerun_failures_alone(app: &Path, logs: &Path, suites: &[Suite], done: &mut [Done]) {
    for d in done.iter_mut() {
        let Outcome::Failed(tail) = &d.outcome else {
            continue;
        };
        let Some((_, env)) = suites.iter().find(|(n, _)| *n == d.name) else {
            continue;
        };
        let log = fs::read_to_string(logs.join(log_name(d.name))).unwrap_or_default();
        let failed = failed_tests(&log);
        if failed.is_empty() || failed.len() > 20 {
            continue; // nothing ran, or a real breakage: no point re-running
        }
        let filter = failed
            .iter()
            .map(|t| format!("test(={t})"))
            .collect::<Vec<_>>()
            .join(" | ");
        let mut cmd = Command::new("cargo");
        cmd.args([
            "nextest",
            "run",
            "--cargo-quiet",
            "--no-fail-fast",
            "--test-threads",
            "1",
            "--status-level",
            "fail",
            "--final-status-level",
            "none",
            "-E",
            &filter,
        ])
        .current_dir(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        if cmd.status().is_ok_and(|st| st.success()) {
            let tail = format!(
                "{tail}\n\nflaky: {} passed when run again alone. It depends on what runs beside \
                 it (shared files under storage/, a port, a timer): fix the test.",
                failed.join(", ")
            );
            println!(
                "  FLAKY {:<22}         {} failed beside the other suite, passed alone",
                d.name,
                failed.join(", ")
            );
            d.note = format!("{} (flaky: {})", d.note, failed.join(", "));
            d.outcome = Outcome::Failed(tail);
        }
    }
}

/// Test names from nextest's `FAIL [ 0.1s] <binary> <test>` lines (retried ones once).
fn failed_tests(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in log.lines() {
        let t = line.trim_start();
        if !(t.starts_with("FAIL [") || t.starts_with("TIMEOUT [")) {
            continue;
        }
        if let Some(name) = t.split_whitespace().last()
            && !out.iter().any(|n| n == name)
        {
            out.push(name.to_owned());
        }
    }
    out
}

/// nextest's `Summary` line, or clippy's warning count, for the one-line report.
fn summary_note(log: &str) -> String {
    if let Some(line) = log
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("Summary ["))
    {
        // "Summary [   4.1s] 652 tests run: 652 passed, 0 skipped" → "652 tests run: 652 passed, 0 skipped"
        return line
            .split_once(']')
            .map_or(line, |(_, rest)| rest)
            .trim()
            .to_owned();
    }
    let warnings = log.lines().filter(|l| l.starts_with("warning:")).count();
    if warnings > 0 {
        return format!("{warnings} warning(s)");
    }
    String::new()
}

/// `cargo <sub> --version` works (an installed cargo subcommand).
fn have_cargo(sub: &str) -> bool {
    Command::new("cargo")
        .args([sub, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn log_name(step: &str) -> String {
    let mut s: String = step
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    format!("{}.log", s.trim_matches('-'))
}

fn target_sub(app: &Path, sub: &str) -> String {
    let base = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app.join("target"));
    base.join(sub).to_string_lossy().into_owned()
}

fn tailwind(app: &Path) -> Option<String> {
    ["bin/tailwindcss", "bin/tailwindcss.exe"]
        .into_iter()
        .map(|p| app.join(p))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// `TEST_DATABASE_URL` from the environment or `.env`; else the compose `db` service's URL when
/// something listens on `localhost:${DB_PORT:-5432}`. Err: why the Postgres run is skipped.
fn postgres_url(app: &Path) -> std::result::Result<String, String> {
    if let Ok(url) = std::env::var("TEST_DATABASE_URL")
        && !url.is_empty()
    {
        return Ok(url);
    }
    if let Some(url) = dotenv_value(&app.join(".env"), "TEST_DATABASE_URL") {
        return Ok(url);
    }
    let port = std::env::var("DB_PORT")
        .ok()
        .or_else(|| dotenv_value(&app.join(".env"), "DB_PORT"))
        .unwrap_or_else(|| "5432".into());
    let listening = format!("localhost:{port}")
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok());
    if listening {
        Ok(format!(
            "postgres://postgres:postgres@localhost:{port}/postgres"
        ))
    } else {
        Err(format!(
            "no TEST_DATABASE_URL and nothing on localhost:{port} (docker compose up -d db); CI runs it"
        ))
    }
}

fn dotenv_value(path: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        let v = rest.trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_owned())
    })
}

/// `postgres://user:secret@host/db` → `postgres://user:…@host/db`.
fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(s), Some(at)) if at > s => {
            let creds = &url[s + 3..at];
            match creds.split_once(':') {
                Some((user, _)) => format!("{}{user}:…{}", &url[..s + 3], &url[at..]),
                None => url.to_owned(),
            }
        }
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_never_reach_the_summary() {
        assert_eq!(
            redact("postgres://postgres:hunter2@localhost:5432/postgres"),
            "postgres://postgres:…@localhost:5432/postgres"
        );
        assert_eq!(
            redact("postgres://me@localhost/db"),
            "postgres://me@localhost/db"
        );
    }

    #[test]
    fn the_nextest_summary_becomes_the_note() {
        let log = "  Starting 3 tests\n     Summary [   1.204s] 3 tests run: 3 passed, 0 skipped\n";
        assert_eq!(summary_note(log), "3 tests run: 3 passed, 0 skipped");
        assert_eq!(summary_note("warning: x\nwarning: y\n"), "2 warning(s)");
    }

    #[test]
    fn failed_tests_are_read_from_nextest_lines() {
        let log = "        FAIL [   0.121s] app::app a_test::one\n  PASS x\n        FAIL [   0.1s] app::app a_test::one\n     TIMEOUT [  60.0s] app::app b::two\n";
        assert_eq!(failed_tests(log), vec!["a_test::one", "b::two"]);
    }

    #[test]
    fn log_names_are_plain() {
        assert_eq!(
            log_name("tests (SQLite) + coverage"),
            "tests-sqlite-coverage.log"
        );
        assert_eq!(log_name("web: biome"), "web-biome.log");
    }

    #[test]
    fn dotenv_values_ignore_comments_and_quotes() {
        let dir = std::env::temp_dir().join(format!("offrails-verify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let env = dir.join(".env");
        fs::write(
            &env,
            "# TEST_DATABASE_URL=nope\nTEST_DATABASE_URL=\"postgres://x@h/db\"\n",
        )
        .unwrap();
        assert_eq!(
            dotenv_value(&env, "TEST_DATABASE_URL").as_deref(),
            Some("postgres://x@h/db")
        );
        assert_eq!(dotenv_value(&env, "DB_PORT"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_step_skips_what_waits_on_it() {
        let dir = std::env::temp_dir().join(format!("offrails-verify-x-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let steps = vec![
            Step::new("broken", &cargo, &["no-such-subcommand-offrails"]),
            Step::new("after broken", &cargo, &["--version"]).after(&["broken"]),
            Step::new("independent", &cargo, &["--version"]),
        ];
        let done = execute(&dir, &dir, steps);
        let by = |n: &str| done.iter().find(|d| d.name == n).unwrap();
        assert!(matches!(by("broken").outcome, Outcome::Failed(_)));
        assert!(matches!(by("after broken").outcome, Outcome::Skipped(_)));
        assert!(matches!(by("independent").outcome, Outcome::Passed));
        let _ = fs::remove_dir_all(&dir);
    }
}
