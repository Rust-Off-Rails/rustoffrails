//! Hot functions: a low-rate CPU sampling profiler (development only), so "which functions burn
//! CPU" has an answer with no annotations at all. Started from the page (or at boot with
//! `DASHBOARD_PROFILER=always`); it samples continuously until paused, or for `MAX_RUN`.
//!
//! WHY on demand and not always on: sampling is cheap, but `pprof` keeps its sample table (about
//! 90 MB) once it has started, and symbolizing a debug build maps its debug info (hundreds of MB,
//! mostly file-backed). Measured on the demo app: 40 MB RSS without it, 440 MB while sampling,
//! 260 MB after stopping (the symbol cache dropped, the table kept). So it runs when you look,
//! says what it costs, and stops by itself. `pprof` (tikv's pprof-rs,
//! signal-based, `--no-default-features`: no C++ demangler) samples every thread `FREQUENCY` times a
//! second; every `WINDOW` the samples are symbolized and folded into per-minute profiles, the last
//! `MINUTES` of them kept in memory (bounded: `MAX_STACKS` distinct stacks a minute, function
//! names interned once).
//!
//! What it can't see: time spent waiting (an `await` on the database, a sleep). Spans measure that
//! (wall clock: the Methods page); this measures CPU.
//!
//! | Platform | Hot functions |
//! |----------|---------------|
//! | Linux, macOS (`cfg(unix)`) | yes: SIGPROF sampling |
//! | Windows | no: spans only (the page says so) |
//!
//! Stacks are function names from the app's binary: no arguments, no data.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::lock;
use super::methods::Frame;

/// Samples per second per thread. APP: 99 (not 100) so it doesn't beat with timers that tick on
/// round numbers.
pub const FREQUENCY: i32 = 99;
/// How often samples are symbolized and folded.
pub const WINDOW: Duration = Duration::from_secs(15);
/// Sampling stops by itself after this long (you'll have forgotten it). APP.
pub const MAX_RUN: Duration = Duration::from_secs(30 * 60);
/// Minutes of profiles kept.
pub const MINUTES: usize = 15;
/// Distinct stacks kept a minute (the rest count as "(other stacks)").
pub const MAX_STACKS: usize = 1_500;
/// Distinct function names kept.
const MAX_NAMES: usize = 50_000;
/// Frames kept per stack (the innermost).
const MAX_DEPTH: usize = 64;

/// One minute of samples: each distinct stack (root first, interned names) and how many times.
pub struct MinuteProfile {
    pub minute: i64,
    pub samples: u64,
    pub stacks: HashMap<Vec<u32>, u64>,
}

#[derive(Default)]
pub struct State {
    names: Vec<Box<str>>,
    index: HashMap<Box<str>, u32>,
    pub minutes: VecDeque<MinuteProfile>,
}

impl State {
    fn intern(&mut self, name: &str) -> u32 {
        if let Some(i) = self.index.get(name) {
            return *i;
        }
        if self.names.len() >= MAX_NAMES {
            return self.intern_fixed("(other)");
        }
        let i = self.names.len() as u32;
        self.names.push(name.into());
        self.index.insert(name.into(), i);
        i
    }

    fn intern_fixed(&mut self, name: &str) -> u32 {
        if let Some(i) = self.index.get(name) {
            return *i;
        }
        let i = self.names.len() as u32;
        self.names.push(name.into());
        self.index.insert(name.into(), i);
        i
    }

    pub fn name(&self, i: u32) -> &str {
        self.names.get(i as usize).map_or("?", |s| s)
    }

    /// Adds samples: `stacks` are names, root first.
    pub fn add(&mut self, minute: i64, stacks: Vec<(Vec<String>, u64)>) {
        let ids: Vec<(Vec<u32>, u64)> = stacks
            .into_iter()
            .map(|(names, n)| (names.iter().map(|s| self.intern(s)).collect(), n))
            .collect();
        let other = self.intern_fixed("(other stacks)");
        if self.minutes.back().is_none_or(|m| m.minute != minute) {
            // INVARIANT: bounded (test: hot_functions_aggregate_self_and_total).
            if self.minutes.len() == MINUTES {
                self.minutes.pop_front();
            }
            self.minutes.push_back(MinuteProfile {
                minute,
                samples: 0,
                stacks: HashMap::new(),
            });
        }
        let Some(m) = self.minutes.back_mut() else {
            return;
        };
        for (stack, n) in ids {
            m.samples += n;
            if m.stacks.len() >= MAX_STACKS && !m.stacks.contains_key(&stack) {
                *m.stacks.entry(vec![other]).or_default() += n;
            } else {
                *m.stacks.entry(stack).or_default() += n;
            }
        }
    }

    pub fn approx_bytes(&self) -> usize {
        self.names.iter().map(|n| n.len() * 2 + 24).sum::<usize>()
            + self
                .minutes
                .iter()
                .flat_map(|m| m.stacks.keys())
                .map(|k| k.len() * 4 + 48)
                .sum::<usize>()
    }
}

pub static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Not sampling (the default until the page starts it).
static PAUSED: AtomicBool = AtomicBool::new(true);
/// The last fold's cost (symbolizing included), µs: the profiler's own overhead, shown.
pub static FOLD_US: AtomicU64 = AtomicU64::new(0);

/// Whether this build can sample (Linux and macOS).
pub const fn available() -> bool {
    cfg!(unix)
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

pub fn set_paused(p: bool) {
    PAUSED.store(p, Ordering::Relaxed);
}

/// Starts the sampler's thread (paused unless `at_boot`): development only, unix only, once.
/// Returns whether it is available to the page.
// SECURITY: never in production or tests, whatever the environment says
// (test: the_profiler_never_runs_outside_development).
pub fn start(config: &crate::config::Config) -> bool {
    if !config.is_development() || !config.dashboard.profiler || !available() {
        return false;
    }
    if RUNNING.swap(true, Ordering::Relaxed) {
        return true;
    }
    PAUSED.store(!config.dashboard.profiler_at_boot, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("dashboard-profiler".into())
        .spawn(sampler::run);
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Relaxed);
    }
    spawned.is_ok()
}

/// Where a function comes from: `app` (this crate), `deps` (another crate), `runtime` (std,
/// core, alloc, Tokio), `native` (C: SQLite, libc, the OS).
pub fn origin(name: &str) -> &'static str {
    // The sampler marks each frame with where its source lives (`a|`, `d|`, `r|`, `n|`).
    if let Some((tag, _)) = name.split_once('|')
        && tag.len() == 1
    {
        return match tag {
            "a" => "app",
            "d" => "deps",
            "r" => "runtime",
            _ => "native",
        };
    }
    let app = env!("CARGO_CRATE_NAME");
    let n = name.trim_start_matches('<');
    if n.starts_with(app) && n[app.len()..].starts_with("::") {
        "app"
    } else if !n.contains("::") {
        "native"
    } else if ["std::", "core::", "alloc::", "tokio::"]
        .iter()
        .any(|p| n.starts_with(p))
    {
        "runtime"
    } else {
        "deps"
    }
}

/// A frame's name without its origin mark.
pub fn plain(name: &str) -> &str {
    match name.split_once('|') {
        Some((tag, rest)) if tag.len() == 1 => rest,
        _ => name,
    }
}

/// Where a frame's source file is: this app's `src/`, a crate from the registry (Tokio counts as
/// the runtime), the standard library, or C (SQLite, libc). WHY by file and not by name: a debug
/// build with `debug = "line-tables-only"` (the host's) names functions without their module
/// path, but every frame still knows its file.
pub fn origin_of_file(file: &str) -> Option<&'static str> {
    if file.is_empty() {
        return None;
    }
    let f = file.replace('\\', "/");
    if f.ends_with(".c") || f.ends_with(".h") || f.ends_with(".cc") || f.ends_with(".cpp") {
        Some("n")
    } else if f.contains("/rustc/") || f.contains("/rustlib/") || f.starts_with("library/") {
        Some("r")
    } else if f.contains("/.cargo/registry/") || f.contains("/.cargo/git/") {
        let krate = f.split("/.cargo/").nth(1).unwrap_or_default();
        if krate.contains("/tokio-") {
            Some("r")
        } else {
            Some("d")
        }
    } else {
        Some("a")
    }
}

/// A demangled symbol, tidied: no hash suffix, and an async fn's or closure's frame named after
/// its function (`lab::load_items::{async_fn#0}` → `lab::load_items`).
pub fn tidy(name: &str) -> String {
    let mut s = name.to_owned();
    // `…::h0123456789abcdef`
    if let Some(at) = s.rfind("::h")
        && s.len() - at == 19
        && s[at + 3..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        s.truncate(at);
    }
    loop {
        let trimmed = s.trim_end();
        match trimmed.rfind("::{") {
            Some(at) if trimmed.ends_with('}') && at > 0 => s.truncate(at),
            _ => break,
        }
    }
    s
}

/// The name without generic arguments: `<a::B<C> as d::E>::f` → `<a::B as d::E>::f`.
fn strip_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0_i32;
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            // A `<` that opens generics follows a name character (`B<C>`); a leading `<` (a
            // trait impl's `<T as Trait>`) is kept.
            '<' if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(*c),
            _ => {}
        }
    }
    out
}

/// One function's numbers over a window.
pub struct Hot {
    pub name: String,
    pub origin: &'static str,
    /// Samples where it was the innermost frame (of the filter's frames).
    pub self_n: u64,
    /// Samples where it was on the stack.
    pub total_n: u64,
    /// The most common path to it (its last few callers).
    pub path: String,
}

/// The window's stacks (root first), filtered: `app`/`deps`/`runtime` keep only those frames.
pub fn stacks(minutes: usize, filter: &str) -> (Vec<(Vec<String>, u64)>, u64) {
    stacks_in(&lock(&STATE), minutes, filter)
}

pub fn stacks_in(st: &State, minutes: usize, filter: &str) -> (Vec<(Vec<String>, u64)>, u64) {
    let mut out: HashMap<Vec<String>, u64> = HashMap::new();
    let mut total = 0;
    for m in st.minutes.iter().rev().take(minutes) {
        total += m.samples;
        for (stack, n) in &m.stacks {
            let names: Vec<String> = stack
                .iter()
                .map(|i| st.name(*i).to_owned())
                .filter(|n| filter == "all" || origin(n) == filter)
                .collect();
            if names.is_empty() {
                continue;
            }
            *out.entry(names).or_default() += n;
        }
    }
    (out.into_iter().collect(), total)
}

/// Hot functions over the last `minutes`, by self samples.
pub fn hot(minutes: usize, filter: &str) -> (Vec<Hot>, u64) {
    let (stacks, total) = stacks(minutes, filter);
    hot_of(&stacks, total)
}

pub fn hot_of(stacks: &[(Vec<String>, u64)], total: u64) -> (Vec<Hot>, u64) {
    let mut by: HashMap<String, Hot> = HashMap::new();
    let mut paths: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (stack, n) in stacks {
        let mut seen = std::collections::HashSet::new();
        for (i, f) in stack.iter().enumerate() {
            let h = by.entry(f.clone()).or_insert_with(|| Hot {
                name: f.clone(),
                origin: origin(f),
                self_n: 0,
                total_n: 0,
                path: String::new(),
            });
            if seen.insert(f.clone()) {
                h.total_n += n;
            }
            if i + 1 == stack.len() {
                h.self_n += n;
            }
            let from = i.saturating_sub(3);
            let p = stack[from..i]
                .iter()
                .map(|s| short(s))
                .collect::<Vec<_>>()
                .join(" → ");
            *paths.entry(f.clone()).or_default().entry(p).or_default() += n;
        }
    }
    let mut v: Vec<Hot> = by
        .into_values()
        .map(|mut h| {
            h.path = paths
                .get(&h.name)
                .and_then(|p| p.iter().max_by_key(|(_, n)| **n).map(|(k, _)| k.clone()))
                .unwrap_or_default();
            h
        })
        .collect();
    v.sort_by(|a, b| {
        b.self_n
            .cmp(&a.self_n)
            .then(b.total_n.cmp(&a.total_n))
            .then(a.name.cmp(&b.name))
    });
    (v, total)
}

/// `a::b::c::d` → `c::d` (generic arguments left out).
pub fn short(name: &str) -> String {
    let name = plain(name);
    // `<a::B<T> as c::D>::poll` → `B::poll`: the implementing type and the method.
    let name = match name.strip_prefix('<') {
        Some(rest) => {
            let mut depth = 1;
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let inner = &rest[..end];
            let ty = inner.split(" as ").next().unwrap_or(inner);
            let tail = rest.get(end + 1..).unwrap_or_default();
            format!("{ty}{tail}")
        }
        None => name.to_owned(),
    };
    let plain = strip_generics(&name);
    let parts: Vec<&str> = plain.split("::").filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        parts.join("::")
    } else {
        parts[parts.len() - 2..].join("::")
    }
}

/// Callers and callees of `name` over the window: (callers, callees), most samples first.
/// Names with their sample counts.
pub type Counts = Vec<(String, u64)>;

pub fn neighbours(minutes: usize, name: &str) -> (Counts, Counts) {
    let (stacks, _) = stacks(minutes, "all");
    let mut up: HashMap<String, u64> = HashMap::new();
    let mut down: HashMap<String, u64> = HashMap::new();
    for (stack, n) in stacks {
        if let Some(i) = stack.iter().position(|f| f == name) {
            if i > 0 {
                *up.entry(stack[i - 1].clone()).or_default() += n;
            }
            if let Some(c) = stack.get(i + 1) {
                *down.entry(c.clone()).or_default() += n;
            }
        }
    }
    let sorted = |m: HashMap<String, u64>| {
        let mut v: Vec<(String, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(15);
        v
    };
    (sorted(up), sorted(down))
}

/// The window as a flame graph (an icicle: root at the top).
pub fn flame(minutes: usize, filter: &str) -> Frame {
    let (stacks, _) = stacks(minutes, filter);
    let mut root = Frame {
        name: "all".into(),
        kind: "request",
        ..Frame::default()
    };
    for (stack, n) in stacks {
        let last = stack.len().saturating_sub(1);
        let mut path: Vec<(String, &'static str)> = Vec::new();
        for (i, s) in stack.into_iter().enumerate() {
            let o = origin(&s);
            let k = match o {
                "app" => "method",
                "deps" => "http",
                "native" => "sql",
                _ => "middleware",
            };
            // A run of frames that aren't the app's (Tokio's and axum's polls, a dependency's
            // internals) is one frame, so your code's shape shows; the innermost frame keeps its
            // own name (that is where the CPU was).
            if o != "app" && i != last {
                let label = match o {
                    "deps" => "(dependencies)",
                    "native" => "(native)",
                    _ => "(std / Tokio)",
                };
                if path.last().is_some_and(|(l, _)| l == label) {
                    continue;
                }
                path.push((label.to_owned(), k));
                continue;
            }
            path.push((short(&s), k));
        }
        root.add(&path, n);
    }
    root
}

#[cfg(unix)]
mod sampler {
    use super::*;

    pub fn run() {
        let mut started: Option<std::time::Instant> = None;
        loop {
            if PAUSED.load(Ordering::Relaxed) {
                if started.take().is_some() {
                    // Stopped: give the debug info's mappings and caches back.
                    backtrace::clear_symbol_cache();
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            let since = *started.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= MAX_RUN {
                PAUSED.store(true, Ordering::Relaxed);
                continue;
            }
            let guard = match pprof::ProfilerGuardBuilder::default()
                .frequency(FREQUENCY)
                .blocklist(&["libc", "libgcc", "pthread", "vdso", "libsystem"])
                .build()
            {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!(error = %e, "the dashboard's CPU profiler couldn't start; hot functions are off");
                    RUNNING.store(false, Ordering::Relaxed);
                    return;
                }
            };
            // Pausing ends the window early.
            let start = std::time::Instant::now();
            while start.elapsed() < WINDOW && !PAUSED.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
            }
            let fold = std::time::Instant::now();
            let report = guard.report().build();
            drop(guard);
            let Ok(report) = report else {
                continue;
            };
            let stacks: Vec<(Vec<String>, u64)> = report
                .data
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(frames, n)| {
                    // pprof lists frames innermost first; the profile keeps the root first.
                    let mut names: Vec<String> = frames
                        .frames
                        .iter()
                        .take(MAX_DEPTH)
                        // A frame lists its inlined functions innermost first; the last is the
                        // real function. Its file says whose code it is.
                        .filter_map(|symbols| {
                            symbols.last().map(|s| {
                                let name = tidy(&s.name());
                                let file = s
                                    .filename
                                    .as_ref()
                                    .map(|f| f.display().to_string())
                                    .unwrap_or_default();
                                let tag = origin_of_file(&file).unwrap_or(match origin(&name) {
                                    "app" => "a",
                                    "deps" => "d",
                                    "runtime" => "r",
                                    _ => "n",
                                });
                                format!("{tag}|{name}")
                            })
                        })
                        .filter(|n| {
                            !plain(n).starts_with("pprof::") && !plain(n).starts_with("backtrace::")
                        })
                        .collect();
                    names.reverse();
                    // An async fn's poll and its body tidy to the same name: one frame.
                    names.dedup();
                    (names, *n as u64)
                })
                .filter(|(names, _)| !names.is_empty())
                .collect();
            let minute = chrono::Utc::now().timestamp().div_euclid(60);
            lock(&STATE).add(minute, stacks);
            FOLD_US.store(fold.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
    }
}

#[cfg(not(unix))]
mod sampler {
    pub fn run() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_functions_aggregate_self_and_total() {
        let mut st = State::default();
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        st.add(
            1,
            vec![
                (s(&["main", "offrails_host::a", "offrails_host::b"]), 6),
                (s(&["main", "offrails_host::a"]), 3),
                (s(&["main", "dep::c"]), 1),
            ],
        );
        let (stacks, total) = stacks_in(&st, MINUTES, "all");
        assert_eq!(total, 10);
        let (hot, _) = hot_of(&stacks, total);
        let b = hot.iter().find(|h| h.name == "offrails_host::b").unwrap();
        let a = hot.iter().find(|h| h.name == "offrails_host::a").unwrap();
        assert_eq!((b.self_n, b.total_n), (6, 6));
        assert_eq!(
            (a.self_n, a.total_n),
            (3, 9),
            "self: innermost; total: on the stack"
        );
        let main = hot.iter().find(|h| h.name == "main").unwrap();
        assert_eq!((main.self_n, main.total_n), (0, 10));
        // Only the app's frames: dep::c's sample disappears, app::a keeps its own.
        let (app, _) = stacks_in(&st, MINUTES, "app");
        assert_eq!(app.iter().map(|(_, n)| n).sum::<u64>(), 9);
        for m in 2..(MINUTES as i64 + 5) {
            st.add(m, Vec::new());
        }
        assert_eq!(st.minutes.len(), MINUTES);
        assert_eq!(tidy("x::y::h0123456789abcdef"), "x::y");
        assert_eq!(
            tidy("app::lab::load_items::{async_fn#0}"),
            "app::lab::load_items"
        );
        assert_eq!(short("a::b::c::d"), "c::d");
        assert_eq!(
            short("std::collections::HashMap<K, V>::insert"),
            "HashMap::insert"
        );
        assert_eq!(origin("sqlite3VdbeExec"), "native");
        assert_eq!(origin("tokio::runtime::park"), "runtime");
        assert_eq!(origin("hyper::proto::h1::role"), "deps");
        assert_eq!(origin("a|is_kernel_path"), "app");
        assert_eq!(plain("n|sqlite3Step"), "sqlite3Step");
        assert_eq!(origin_of_file("src/kernel/ui.rs"), Some("a"));
        assert_eq!(
            origin_of_file("/home/u/.cargo/registry/src/index.crates.io-x/tokio-1.53.3/src/a.rs"),
            Some("r")
        );
        assert_eq!(
            origin_of_file(
                "/home/u/.cargo/registry/src/index.crates.io-x/libsqlite3-sys-0.30/sqlite3/sqlite3.c"
            ),
            Some("n")
        );
        assert_eq!(
            origin_of_file("/rustc/abc/library/core/src/iter.rs"),
            Some("r")
        );
    }
}
