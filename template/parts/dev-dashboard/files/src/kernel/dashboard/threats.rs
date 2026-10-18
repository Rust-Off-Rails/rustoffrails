//! Security signals: deterministic detectors over the request stream (the dashboard's middleware
//! sees every request, before the Host check, CSRF and the body limit answer) and the auth
//! samples' events (an optional wire from auth-core's `auth_event::record`, or the lab's own
//! sign-in form): scanners and exploit probes, path traversal, SQL injection and XSS in query
//! strings, odd methods, oversized requests, bad Host headers, CSRF rejections, CSP report
//! spikes, rate-limit hits, brute force on one account, credential stuffing from one address,
//! one address that is too busy, fails too often, rotates user agents or spans many sessions, and
//! one session used from several addresses.
//!
//! Each finding has its evidence (the requests, with their traces), a severity and what to do.
//! Every threshold is an `// APP:` constant below.
//!
//! **Privacy.** A client address is personal data. Each one is keyed by a keyed hash (SipHash with
//! this process's random key) and shown in production only as its network (`203.0.113.x`,
//! `2001:db8:85a3::/48`) with 6 digits of that hash; development (this machine) shows it whole.
//! The production view never holds a full address. Query strings are inspected in flight and
//! never kept (only which pattern matched); paths are redacted (`kernel::redact::path`); user
//! agents are kept as a family (`Chrome`, `curl`, `sqlmap`) and a hash; session cookies and
//! accounts only as keyed hashes.
//!
//! GUIDE: .offrails/guides/security.md

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::hash::{BuildHasher, RandomState};
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};

use askama::Template;
use axum::Extension;
use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::response::Response;
use chrono::{DateTime, Utc};

use super::insights::{Finding, Severity};
use super::web::{Dash, Mode, chrome, dash};
use super::{HUB, ai, lock};
use crate::config::Config;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::ThemeSwitch;
use crate::kernel::ui;
use crate::state::AppState;

// ---------------------------------------------------------------------------------------------
// Thresholds (APP: tune them to your traffic)
// ---------------------------------------------------------------------------------------------

/// The window every detector looks at, in seconds.
pub const WINDOW_SECS: i64 = 10 * 60; // APP
/// One address with this many 404s in the window is scanning.
pub const SCAN_404S: u32 = 20; // APP
/// One address requesting this many known probe paths is scanning.
pub const PROBE_HITS: u32 = 3; // APP
/// Traversal, SQL injection or XSS attempts from one address before it is a finding.
pub const INJECTION_HITS: u32 = 1; // APP
/// Failed sign-ins for one account in the window: brute force.
pub const BRUTE_FAILURES: u32 = 10; // APP
/// Distinct accounts one address failed to sign in to: credential stuffing.
pub const STUFFING_ACCOUNTS: u32 = 5; // APP
/// Without accounts to tell apart (no auth hook), failed sign-ins from one address.
pub const STUFFING_FAILURES: u32 = 15; // APP
/// Requests in the last minute from one address past both this floor…
pub const RATE_FLOOR: u32 = 120; // APP
/// …and this many times the median address's.
pub const RATE_FACTOR: u32 = 10; // APP
/// 4xx answers to one address in the window, when they are most of its requests.
pub const IP_4XX: u32 = 30; // APP
/// Distinct user agents from one address.
pub const UA_ROTATION: usize = 5; // APP
/// Distinct sessions (or accounts) from one address.
pub const IP_SESSIONS: usize = 8; // APP
/// Distinct addresses (or user agents) one session came from in the window.
pub const SESSION_IPS: usize = 3; // APP
/// CSRF rejections from one address.
pub const CSRF_HITS: u32 = 5; // APP
/// 429s to one address.
pub const RATE_LIMITED: u32 = 10; // APP
/// CSP reports in one minute: a spike (an injected script, or a broken deploy).
pub const CSP_SPIKE: u32 = 20; // APP
/// A request body past this (bytes, by Content-Length), or headers past `BIG_HEADERS`.
pub const BIG_BODY: u64 = 2 * 1024 * 1024; // APP: the app's DefaultBodyLimit
pub const BIG_HEADERS: usize = 16 * 1024; // APP

/// APP: where the app signs people in (the fallback without an auth hook: a POST answered 401,
/// 422 or 429 there is a failed sign-in).
pub const SIGN_IN_ROUTES: &[&str] = &[
    "/session",
    "/session/password",
    "/login",
    "/sign_in",
    "/users/sign_in",
    "/dev/lab/sign-in",
];

/// APP: paths no legitimate visitor of this app asks for (lowercase; a prefix match).
pub const PROBE_PATHS: &[&str] = &[
    "/.env",
    "/.git",
    "/.svn",
    "/.hg",
    "/.aws",
    "/.ssh",
    "/.ds_store",
    "/.htaccess",
    "/.htpasswd",
    "/wp-admin",
    "/wp-login.php",
    "/wp-content",
    "/wp-includes",
    "/xmlrpc.php",
    "/phpmyadmin",
    "/pma",
    "/myadmin",
    "/server-status",
    "/server-info",
    "/actuator",
    "/cgi-bin",
    "/vendor/phpunit",
    "/config.php",
    "/config.json",
    "/admin.php",
    "/backup",
    "/boaform",
    "/hnap1",
    "/solr",
    "/console",
    "/druid",
    "/telescope",
    "/_profiler",
];

/// APP: user agents of known scanners (lowercase substrings).
pub const SCANNER_AGENTS: &[&str] = &[
    "sqlmap",
    "nikto",
    "nuclei",
    "masscan",
    "zgrab",
    "nmap",
    "wpscan",
    "dirbuster",
    "gobuster",
    "ffuf",
    "acunetix",
    "nessus",
    "openvas",
    "w3af",
    "havij",
    "jaeles",
    "feroxbuster",
];

const TRAVERSAL: &[&str] = &[
    "../",
    "..\\",
    "/etc/passwd",
    "/etc/shadow",
    "c:\\windows",
    "win.ini",
    "/proc/self",
];
const SQLI: &[&str] = &[
    "union select",
    "union all select",
    "' or '1'='1",
    "' or 1=1",
    "\" or \"1\"=\"1",
    " or 1=1--",
    "'--",
    "sleep(",
    "benchmark(",
    "pg_sleep",
    "waitfor delay",
    "information_schema",
    "; drop table",
    "xp_cmdshell",
];
const XSS: &[&str] = &[
    "<script",
    "javascript:",
    "onerror=",
    "onload=",
    "<svg",
    "<img",
    "<iframe",
    "alert(",
    "document.cookie",
];
const ODD_METHODS: &[&str] = &[
    "TRACE",
    "TRACK",
    "PROPFIND",
    "PROPPATCH",
    "MKCOL",
    "CONNECT",
    "DEBUG",
    "SEARCH",
];

/// Flagged requests kept for evidence.
pub const MAX_EVENTS: usize = 2_000;
/// Addresses, sessions and accounts tracked (the least recently seen goes first).
pub const MAX_IPS: usize = 1_000;
pub const MAX_SESSIONS: usize = 1_000;
pub const MAX_ACCOUNTS: usize = 1_000;
/// Per address: distinct user agents, sessions and accounts kept.
const MAX_SET: usize = 64;

// ---------------------------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------------------------

/// What one request showed (bit flags).
pub mod sig {
    pub const NOT_FOUND: u32 = 1;
    pub const PROBE: u32 = 1 << 1;
    pub const SCANNER: u32 = 1 << 2;
    pub const TRAVERSAL: u32 = 1 << 3;
    pub const SQLI: u32 = 1 << 4;
    pub const XSS: u32 = 1 << 5;
    pub const ODD_METHOD: u32 = 1 << 6;
    pub const BIG_BODY: u32 = 1 << 7;
    pub const BIG_HEADERS: u32 = 1 << 8;
    pub const BAD_HOST: u32 = 1 << 9;
    pub const CSRF: u32 = 1 << 10;
    pub const RATE_LIMITED: u32 = 1 << 11;
    pub const SIGN_IN_FAILED: u32 = 1 << 12;
    pub const CLIENT_ERROR: u32 = 1 << 13;
    /// Flags that make a request worth keeping as evidence.
    pub const NOTABLE: u32 = !(NOT_FOUND | CLIENT_ERROR);

    /// (flag, label, category) for every flag.
    pub const ALL: [(u32, &str, &str); 14] = [
        (NOT_FOUND, "404", "probe"),
        (PROBE, "probe path", "probe"),
        (SCANNER, "scanner user agent", "probe"),
        (TRAVERSAL, "path traversal", "injection"),
        (SQLI, "SQL injection", "injection"),
        (XSS, "XSS", "injection"),
        (ODD_METHOD, "unusual method", "probe"),
        (BIG_BODY, "oversized body", "abuse"),
        (BIG_HEADERS, "oversized headers", "abuse"),
        (BAD_HOST, "bad Host header", "abuse"),
        (CSRF, "CSRF rejection", "abuse"),
        (RATE_LIMITED, "rate limited (429)", "abuse"),
        (SIGN_IN_FAILED, "failed sign-in", "auth"),
        (CLIENT_ERROR, "4xx", "abuse"),
    ];

    pub fn labels(flags: u32) -> Vec<&'static str> {
        ALL.iter()
            .filter(|(f, _, _)| flags & f != 0)
            .map(|(_, l, _)| *l)
            .collect()
    }

    /// The category a request counts under on the timeline (the most serious flag wins).
    pub fn category(flags: u32) -> &'static str {
        if flags & (TRAVERSAL | SQLI | XSS) != 0 {
            "injection"
        } else if flags & SIGN_IN_FAILED != 0 {
            "auth"
        } else if flags & (PROBE | SCANNER | ODD_METHOD) != 0 {
            "probe"
        } else {
            "abuse"
        }
    }
}

/// What the middleware needs from the config, set by `instrument` (the middleware has no state).
pub struct Setup {
    pub trusted_proxy: bool,
    pub production: bool,
    pub development: bool,
    pub hosts: crate::kernel::hosts::AllowedHosts,
}

static SETUP: Mutex<Option<Arc<Setup>>> = Mutex::new(None);

/// This process's hash key: addresses, sessions, user agents and accounts are keyed by it.
static KEY: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// The simulator's proof that a request is its own (and may carry a simulated client address):
/// random per process, never shown.
pub static SIM_SECRET: LazyLock<String> =
    LazyLock::new(|| format!("{:032x}", rand::random::<u128>()));
/// The headers the simulator sends: its secret, and the address to attribute the request to.
pub const SIM_HEADER: &str = "x-offrails-lab";
pub const SIM_CLIENT: &str = "x-offrails-lab-client";

pub fn configure(config: &Config) {
    *lock(&SETUP) = Some(Arc::new(Setup {
        trusted_proxy: config.trusted_proxy,
        production: config.is_production(),
        development: config.is_development(),
        hosts: crate::kernel::hosts::AllowedHosts::new(config),
    }));
}

fn setup() -> Option<Arc<Setup>> {
    lock(&SETUP).clone()
}

pub fn keyed(v: impl std::hash::Hash) -> u64 {
    KEY.hash_one(v)
}

/// A client address as it may be shown: whole in development, its network and a keyed hash in
/// production.
// SECURITY: the production view never holds or shows a full client address, only its /24 (/48
// for IPv6) and 6 hex digits of a keyed hash (test: client_addresses_are_truncated_in_production).
pub fn ip_label(ip: Option<IpAddr>, production: bool) -> String {
    let Some(ip) = ip else {
        return "unknown (no socket)".to_owned();
    };
    if !production {
        return ip.to_string();
    }
    let h = keyed(ip) & 0xff_ffff;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.x #{h:06x}", o[0], o[1], o[2])
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}::/48 #{h:06x}", s[0], s[1], s[2])
        }
    }
}

/// A user agent as kept: its family (never the string itself, which carries versions, devices
/// and, from scripts, whatever the attacker typed).
pub fn ua_family(ua: &str) -> &'static str {
    let l = ua.to_ascii_lowercase();
    if let Some(s) = SCANNER_AGENTS.iter().find(|s| l.contains(*s)) {
        return s;
    }
    for (needle, family) in [
        ("offrails-load-lab", "load lab"),
        ("curl/", "curl"),
        ("wget/", "wget"),
        ("python-requests", "python-requests"),
        ("python-urllib", "python-urllib"),
        ("go-http-client", "Go http client"),
        ("httpie", "HTTPie"),
        ("postman", "Postman"),
        ("headlesschrome", "headless Chrome"),
        ("bot", "a bot"),
        ("edg/", "Edge"),
        ("firefox/", "Firefox"),
        ("chrome/", "Chrome"),
        ("safari/", "Safari"),
    ] {
        if l.contains(needle) {
            return family;
        }
    }
    if ua.is_empty() { "(none)" } else { "other" }
}

/// User agents' families by hash: a client sends the same one on every request. Bounded.
static FAMILIES: LazyLock<Mutex<HashMap<u64, &'static str>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached_family(hash: u64, ua: &str) -> &'static str {
    if let Some(f) = lock(&FAMILIES).get(&hash) {
        return f;
    }
    let f = ua_family(ua);
    let mut map = lock(&FAMILIES);
    if map.len() >= 512 {
        map.clear();
    }
    map.insert(hash, f);
    f
}

/// One percent-decoding pass (bounded: a URL past 4 KB is checked as is).
fn decode(s: &str) -> String {
    if s.len() > 4096 || (!s.contains('%') && !s.contains('+')) {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (
                    b.get(i + 1).and_then(|c| hex(*c)),
                    b.get(i + 2).and_then(|c| hex(*c)),
                ) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The attack patterns in a path and query string (decoded twice: `%252e%252e%252f` is `../`).
pub fn patterns(path: &str, query: &str) -> u32 {
    let mut flags = 0;
    // WHY no allocation on this path: it runs on every request (test: the_middleware_overhead_is_small).
    if PROBE_PATHS.iter().any(|p| {
        path.len() >= p.len()
            && path.is_char_boundary(p.len())
            && path[..p.len()].eq_ignore_ascii_case(p)
            && {
                let rest = &path[p.len()..];
                rest.is_empty()
                    || rest.starts_with(['/', '.', '?'])
                    || p.ends_with('/')
                    || p.contains('.')
            }
    }) {
        flags |= sig::PROBE;
    }
    for raw in [path, query] {
        // Only text with a character an attack needs is decoded and scanned.
        let suspicious = raw.bytes().any(|b| {
            matches!(
                b,
                b'%' | b'<' | b'>' | b'\'' | b'"' | b'\\' | b'(' | b';' | b' ' | b'+' | b':'
            )
        }) || raw.contains("..")
            || raw.contains("/etc/");
        if raw.is_empty() || !suspicious {
            continue;
        }
        let once = decode(raw);
        let twice = decode(&once).to_ascii_lowercase();
        let once = once.to_ascii_lowercase();
        let hit = |list: &[&str]| list.iter().any(|p| once.contains(p) || twice.contains(p));
        if hit(TRAVERSAL) || raw.to_ascii_lowercase().contains("%2e%2e") {
            flags |= sig::TRAVERSAL;
        }
        if hit(SQLI) {
            flags |= sig::SQLI;
        }
        if hit(XSS) {
            flags |= sig::XSS;
        }
    }
    flags
}

/// What the middleware read from a request before running it (the request is gone after).
pub struct Pending {
    ip: Option<IpAddr>,
    /// What the request itself showed (before its answer).
    pub flags: u32,
    method: &'static str,
    unsafe_method: bool,
    cross_site: bool,
    bad_host: bool,
    ua_hash: u64,
    ua: &'static str,
    session: Option<u64>,
    sign_in_route: bool,
}

fn header_str<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

/// The client's address: the socket, or the rightmost `X-Forwarded-For` hop behind a trusted
/// proxy, or (development, the attack simulator's own requests only) the address it simulates.
// SECURITY: a simulated address is honored only with this process's random secret, in
// development, so no client can choose the address it is recorded as
// (test: a_simulated_client_address_needs_the_simulators_secret).
fn client_ip(req: &Request, s: &Setup) -> Option<IpAddr> {
    if s.development
        && header_str(req, SIM_HEADER).is_some_and(|v| v == SIM_SECRET.as_str())
        && let Some(ip) = header_str(req, SIM_CLIENT).and_then(|v| v.trim().parse().ok())
    {
        return Some(ip);
    }
    let forwarded = s
        .trusted_proxy
        .then(|| {
            header_str(req, "x-forwarded-for")
                .and_then(|v| v.rsplit(',').next())
                .and_then(|hop| hop.trim().parse::<IpAddr>().ok())
        })
        .flatten();
    forwarded.or_else(|| {
        req.extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip())
    })
}

/// The simulated address on one of the simulator's own requests (development only, with the
/// secret); `None` for every other request.
pub fn lab_client(headers: &axum::http::HeaderMap) -> Option<String> {
    let s = setup()?;
    let ok = s.development
        && headers
            .get(SIM_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == SIM_SECRET.as_str());
    ok.then(|| {
        headers
            .get(SIM_CLIENT)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
            .map(|ip| ip.to_string())
    })
    .flatten()
}

/// Reads what the detectors need from a request (the middleware calls it before the handler).
pub fn inspect(req: &Request, route: &'static str) -> Option<Pending> {
    let s = setup()?;
    let path = req.uri().path();
    let query = req.uri().query().unwrap_or_default();
    let mut flags = patterns(path, query);
    let method = req.method();
    let m: &'static str = match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => match ODD_METHODS.iter().find(|o| **o == method.as_str()) {
            Some(o) => {
                flags |= sig::ODD_METHOD;
                o
            }
            None => "OTHER",
        },
    };
    let ua = header_str(req, "user-agent").unwrap_or_default();
    let ua_hash = keyed(("ua", ua));
    let family = cached_family(ua_hash, ua);
    if SCANNER_AGENTS.contains(&family) {
        flags |= sig::SCANNER;
    }
    if header_str(req, "content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > BIG_BODY)
    {
        flags |= sig::BIG_BODY;
    }
    let header_bytes: usize = req
        .headers()
        .iter()
        .map(|(k, v)| k.as_str().len() + v.len())
        .sum();
    if header_bytes > BIG_HEADERS {
        flags |= sig::BIG_HEADERS;
    }
    let host = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .or_else(|| header_str(req, "host"));
    let unsafe_method = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let cross_site = header_str(req, "sec-fetch-site").is_some_and(|v| v == "cross-site")
        || header_str(req, "origin").is_some_and(|o| {
            let authority = o.split("://").nth(1).unwrap_or(o);
            host.is_none_or(|h| !h.eq_ignore_ascii_case(authority))
        });
    let session = req
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|c| c.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| {
            *k == crate::kernel::security::SESSION_COOKIE_DEV
                || *k == crate::kernel::security::SESSION_COOKIE_PROD
                || *k == "lab_session"
        })
        .map(|(_, v)| keyed(("session", v)));
    Some(Pending {
        ip: client_ip(req, &s),
        flags,
        method: m,
        unsafe_method,
        cross_site,
        bad_host: !s.hosts.allows(host),
        ua_hash,
        ua: family,
        session,
        sign_in_route: SIGN_IN_ROUTES.contains(&route),
    })
}

/// The response came back: classify by its status, then record.
pub fn finish(p: Pending, status: u16, path: &str, trace: u64) {
    let Some(s) = setup() else {
        return;
    };
    let mut flags = p.flags;
    match status {
        404 => flags |= sig::NOT_FOUND,
        429 => flags |= sig::RATE_LIMITED,
        413 => flags |= sig::BIG_BODY,
        403 if p.bad_host => flags |= sig::BAD_HOST,
        403 if p.unsafe_method && p.cross_site => flags |= sig::CSRF,
        _ => {}
    }
    if (400..500).contains(&status) {
        flags |= sig::CLIENT_ERROR;
    }
    // WHY always, even with the auth hook: it is the evidence; `findings` takes the larger of the
    // two counts, never their sum.
    if p.sign_in_route && p.unsafe_method && matches!(status, 401 | 422 | 429) {
        flags |= sig::SIGN_IN_FAILED;
    }
    let ip = p.ip.map_or(0, keyed);
    let notable = flags & sig::NOTABLE != 0;
    let mut t = lock(&HUB.threats);
    // WHY: the label and the path are built only when something will keep them (a new address,
    // a flagged request): most requests allocate nothing here.
    let obs = Obs {
        at: Utc::now().timestamp(),
        ip,
        ip_label: if notable || !t.ips.contains_key(&ip) {
            ip_label(p.ip, s.production)
        } else {
            String::new()
        },
        method: if notable {
            p.method.to_owned()
        } else {
            String::new()
        },
        path: if notable {
            clean_path(path)
        } else {
            String::new()
        },
        status,
        flags,
        ua_hash: p.ua_hash,
        ua: p.ua,
        session: p.session,
        local: p.ip.is_some_and(|ip| ip.is_loopback()),
        trace,
    };
    t.ingest(obs);
}

/// A path as kept: redacted (tokens), control characters out, cut.
// SECURITY: a path an attacker chose can carry a token, markup or terminal escapes; it is
// redacted and stripped of control characters before it is stored, and Askama escapes it on the
// page (test: hostile_paths_and_agents_are_stored_inert).
pub fn clean_path(path: &str) -> String {
    let redacted = crate::kernel::redact::path(path);
    let printable: String = redacted
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    ui::truncate(&printable, 120)
}

/// One observed request.
#[derive(Clone, Debug)]
pub struct Obs {
    /// Unix seconds.
    pub at: i64,
    /// The keyed hash of the client address (0: unknown).
    pub ip: u64,
    pub ip_label: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub flags: u32,
    pub ua_hash: u64,
    pub ua: &'static str,
    pub session: Option<u64>,
    /// From this machine (the developer, the load lab): not judged on volume or variety.
    pub local: bool,
    /// Its trace (0: none).
    pub trace: u64,
}

impl Obs {
    /// A synthetic request (tests, the demo).
    pub fn synthetic(at: i64, ip: &str, method: &str, path: &str, status: u16, flags: u32) -> Self {
        let addr: Option<IpAddr> = ip.parse().ok();
        Self {
            at,
            ip: addr.map_or(0, keyed),
            ip_label: ip_label(addr, false),
            method: method.to_owned(),
            path: clean_path(path),
            status,
            flags: flags
                | patterns(
                    path.split('?').next().unwrap_or(path),
                    path.split_once('?').map_or("", |(_, q)| q),
                )
                | if status == 404 { sig::NOT_FOUND } else { 0 }
                | if (400..500).contains(&status) {
                    sig::CLIENT_ERROR
                } else {
                    0
                },
            ua_hash: keyed(("ua", "test")),
            ua: "other",
            session: None,
            local: addr.is_some_and(|a| a.is_loopback()),
            trace: 0,
        }
    }
}

/// An auth event (the auth hook): a failed sign-in, a pause, a success.
#[derive(Clone, Debug)]
pub struct AuthObs {
    pub at: i64,
    pub ip: u64,
    pub ip_label: String,
    /// The keyed hash of the account (None: an address with no account).
    pub account: Option<u64>,
    /// `failed`, `throttled` or `signed_in`.
    pub kind: &'static str,
}

/// The auth samples report here (auth-core's `auth_event::record`, through its optional wire;
/// the lab's sign-in form). `kind` is the event's name (`SignInFailed`, `Throttled`,
/// `SignedIn`, `TwoFactorFailed`, …); others are ignored.
pub fn auth_event(kind: &str, account: Option<i64>, ip: Option<&str>) {
    if !HUB.enabled() {
        return;
    }
    let kind = match kind {
        "SignInFailed" | "TwoFactorFailed" | "sign_in_failed" | "two_factor_failed" => "failed",
        "Throttled" | "throttled" => "throttled",
        "SignedIn" | "signed_in" => "signed_in",
        _ => return,
    };
    let production = setup().is_some_and(|s| s.production);
    let addr: Option<IpAddr> = ip.and_then(|i| i.parse().ok());
    lock(&HUB.threats).ingest_auth(AuthObs {
        at: Utc::now().timestamp(),
        ip: addr.map_or(0, keyed),
        ip_label: ip_label(addr, production),
        account: account.map(|a| keyed(("account", a))),
        kind,
    });
}

/// The lab's sign-in form (and any app without accounts) names the account by the address it
/// was tried with: only its keyed hash is kept.
pub fn auth_event_for(kind: &str, account: &str, ip: Option<&str>) {
    if !HUB.enabled() {
        return;
    }
    let production = setup().is_some_and(|s| s.production);
    let addr: Option<IpAddr> = ip.and_then(|i| i.parse().ok());
    let kind = match kind {
        "SignInFailed" => "failed",
        "Throttled" => "throttled",
        "SignedIn" => "signed_in",
        _ => return,
    };
    lock(&HUB.threats).ingest_auth(AuthObs {
        at: Utc::now().timestamp(),
        ip: addr.map_or(0, keyed),
        ip_label: ip_label(addr, production),
        account: (!account.is_empty())
            .then(|| keyed(("account-name", account.to_ascii_lowercase()))),
        kind,
    });
}

/// A CSP report arrived (the spike detector counts them per minute).
pub fn csp_report() {
    if HUB.enabled() {
        lock(&HUB.threats).csp(Utc::now().timestamp());
    }
}

// ---------------------------------------------------------------------------------------------
// The state: bounded rings and maps, and the detectors over them
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct IpStats {
    pub label: String,
    pub first: i64,
    pub last: i64,
    /// (unix minute, requests) for the last `WINDOW_SECS`.
    pub minutes: VecDeque<(i64, u32)>,
    /// Flag counts in the window: (unix second, flags), at most `MAX_EVENTS / 4` per address.
    pub hits: VecDeque<(i64, u32)>,
    pub uas: Vec<(u64, &'static str)>,
    pub sessions: Vec<u64>,
    pub accounts: Vec<u64>,
    pub auth_failed: VecDeque<i64>,
    pub signed_in_after_failures: u32,
    /// A loopback address (this machine).
    pub local: bool,
}

impl IpStats {
    fn requests(&self, since: i64) -> u32 {
        self.minutes
            .iter()
            .filter(|(m, _)| m * 60 >= since - 59)
            .map(|(_, n)| n)
            .sum()
    }

    fn count(&self, flag: u32, since: i64) -> u32 {
        self.hits
            .iter()
            .filter(|(t, f)| *t >= since && f & flag != 0)
            .count() as u32
    }
}

#[derive(Default, Clone)]
pub struct SessionStats {
    pub last: i64,
    /// (address hash, label, when).
    pub ips: Vec<(u64, String, i64)>,
    pub uas: Vec<(u64, i64)>,
}

#[derive(Default, Clone)]
pub struct AccountStats {
    pub last: i64,
    pub failed: VecDeque<i64>,
    pub throttled: u32,
    pub ips: Vec<(u64, String)>,
}

#[derive(Default)]
pub struct Threats {
    events: VecDeque<Obs>,
    ips: HashMap<u64, IpStats>,
    sessions: HashMap<u64, SessionStats>,
    accounts: HashMap<u64, AccountStats>,
    auth: VecDeque<AuthObs>,
    /// (unix minute, CSP reports).
    csp: VecDeque<(i64, u32)>,
    /// (unix minute, [probe, injection, auth, abuse]) for the timeline, the last hour.
    timeline: VecDeque<(i64, [u32; 4])>,
    pub requests: u64,
}

fn evict_oldest<K: Copy + Eq + std::hash::Hash, V>(
    map: &mut HashMap<K, V>,
    last: impl Fn(&V) -> i64,
) {
    if let Some(k) = map.iter().min_by_key(|(_, v)| last(v)).map(|(k, _)| *k) {
        map.remove(&k);
    }
}

fn push_set<T: PartialEq>(v: &mut Vec<T>, x: T) {
    if !v.contains(&x) && v.len() < MAX_SET {
        v.push(x);
    }
}

const CATEGORIES: [&str; 4] = ["probe", "injection", "auth", "abuse"];

impl Threats {
    pub fn ingest(&mut self, o: Obs) {
        self.requests += 1;
        let minute = o.at.div_euclid(60);
        // INVARIANT: every map and ring is bounded (test: the_signals_stay_within_their_memory_bound).
        if !self.ips.contains_key(&o.ip) && self.ips.len() >= MAX_IPS {
            evict_oldest(&mut self.ips, |s| s.last);
        }
        let ip = self.ips.entry(o.ip).or_insert_with(|| IpStats {
            label: o.ip_label.clone(),
            first: o.at,
            local: o.local,
            ..IpStats::default()
        });
        ip.last = o.at;
        match ip.minutes.back_mut() {
            Some(m) if m.0 == minute => m.1 += 1,
            _ => ip.minutes.push_back((minute, 1)),
        }
        while ip
            .minutes
            .front()
            .is_some_and(|m| m.0 * 60 < o.at - WINDOW_SECS)
        {
            ip.minutes.pop_front();
        }
        if o.flags != 0 {
            ip.hits.push_back((o.at, o.flags));
            if ip.hits.len() > MAX_EVENTS / 4 {
                ip.hits.pop_front();
            }
        }
        while ip.hits.front().is_some_and(|h| h.0 < o.at - WINDOW_SECS) {
            ip.hits.pop_front();
        }
        push_set(&mut ip.uas, (o.ua_hash, o.ua));
        if let Some(sess) = o.session {
            push_set(&mut ip.sessions, sess);
            if !self.sessions.contains_key(&sess) && self.sessions.len() >= MAX_SESSIONS {
                evict_oldest(&mut self.sessions, |s| s.last);
            }
            let st = self.sessions.entry(sess).or_default();
            st.last = o.at;
            st.ips.retain(|(_, _, t)| *t >= o.at - WINDOW_SECS);
            st.uas.retain(|(_, t)| *t >= o.at - WINDOW_SECS);
            if let Some(x) = st.ips.iter_mut().find(|(h, _, _)| *h == o.ip) {
                x.2 = o.at;
            } else if st.ips.len() < MAX_SET {
                st.ips.push((o.ip, ip.label.clone(), o.at));
            }
            if let Some(x) = st.uas.iter_mut().find(|(h, _)| *h == o.ua_hash) {
                x.1 = o.at;
            } else if st.uas.len() < MAX_SET {
                st.uas.push((o.ua_hash, o.at));
            }
        }
        if o.flags & sig::NOTABLE != 0 {
            let cat = CATEGORIES
                .iter()
                .position(|c| *c == sig::category(o.flags))
                .unwrap_or(3);
            match self.timeline.back_mut() {
                Some(t) if t.0 == minute => t.1[cat] += 1,
                _ => {
                    let mut counts = [0; 4];
                    counts[cat] = 1;
                    self.timeline.push_back((minute, counts));
                }
            }
            while self.timeline.len() > 60 {
                self.timeline.pop_front();
            }
            if self.events.len() == MAX_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(o);
        }
    }

    pub fn ingest_auth(&mut self, a: AuthObs) {
        let minute = a.at.div_euclid(60);
        if !self.ips.contains_key(&a.ip) && self.ips.len() >= MAX_IPS {
            evict_oldest(&mut self.ips, |s| s.last);
        }
        let ip = self.ips.entry(a.ip).or_insert_with(|| IpStats {
            label: a.ip_label.clone(),
            first: a.at,
            ..IpStats::default()
        });
        ip.last = ip.last.max(a.at);
        while ip
            .auth_failed
            .front()
            .is_some_and(|t| *t < a.at - WINDOW_SECS)
        {
            ip.auth_failed.pop_front();
        }
        match a.kind {
            "failed" => {
                ip.auth_failed.push_back(a.at);
                if ip.auth_failed.len() > 1_000 {
                    ip.auth_failed.pop_front();
                }
                if let Some(acct) = a.account {
                    push_set(&mut ip.accounts, acct);
                }
            }
            "signed_in" if ip.auth_failed.len() as u32 >= BRUTE_FAILURES => {
                ip.signed_in_after_failures += 1;
            }
            _ => {}
        }
        if let Some(acct) = a.account {
            if !self.accounts.contains_key(&acct) && self.accounts.len() >= MAX_ACCOUNTS {
                evict_oldest(&mut self.accounts, |s| s.last);
            }
            let st = self.accounts.entry(acct).or_default();
            st.last = a.at;
            while st.failed.front().is_some_and(|t| *t < a.at - WINDOW_SECS) {
                st.failed.pop_front();
            }
            match a.kind {
                "failed" => {
                    st.failed.push_back(a.at);
                    if st.failed.len() > 1_000 {
                        st.failed.pop_front();
                    }
                }
                "throttled" => st.throttled += 1,
                _ => {}
            }
            if !st.ips.iter().any(|(h, _)| *h == a.ip) && st.ips.len() < MAX_SET {
                st.ips.push((a.ip, a.ip_label.clone()));
            }
        }
        if a.kind == "failed" {
            match self.timeline.back_mut() {
                Some(t) if t.0 == minute => t.1[2] += 1,
                _ => self.timeline.push_back((minute, [0, 0, 1, 0])),
            }
            while self.timeline.len() > 60 {
                self.timeline.pop_front();
            }
        }
        if self.auth.len() == MAX_EVENTS {
            self.auth.pop_front();
        }
        self.auth.push_back(a);
    }

    pub fn csp(&mut self, at: i64) {
        let minute = at.div_euclid(60);
        match self.csp.back_mut() {
            Some(c) if c.0 == minute => c.1 += 1,
            _ => self.csp.push_back((minute, 1)),
        }
        while self.csp.len() > 60 {
            self.csp.pop_front();
        }
    }

    pub fn approx_bytes(&self) -> usize {
        self.events
            .iter()
            .map(|e| std::mem::size_of::<Obs>() + e.path.len() + e.ip_label.len() + e.method.len())
            .sum::<usize>()
            + self
                .ips
                .values()
                .map(|s| {
                    std::mem::size_of::<IpStats>()
                        + s.label.len()
                        + s.minutes.len() * 16
                        + s.hits.len() * 16
                        + s.uas.len() * 24
                        + (s.sessions.len() + s.accounts.len() + s.auth_failed.len()) * 8
                })
                .sum::<usize>()
            + self.sessions.len() * (std::mem::size_of::<SessionStats>() + 64)
            + self.accounts.len() * (std::mem::size_of::<AccountStats>() + 64)
            + self.auth.len() * std::mem::size_of::<AuthObs>()
    }

    pub fn lens(&self) -> (usize, usize, usize, usize) {
        (
            self.events.len(),
            self.ips.len(),
            self.sessions.len(),
            self.accounts.len(),
        )
    }

    /// The flagged requests of one address (newest first).
    fn evidence(&self, ip: u64, flag: u32, limit: usize) -> Vec<Evidence> {
        self.events
            .iter()
            .rev()
            .filter(|e| e.ip == ip && e.flags & flag != 0)
            .take(limit)
            .map(Evidence::of)
            .collect()
    }

    /// The timeline: (unix minute, [probe, injection, auth, abuse]) for the last `minutes`.
    pub fn timeline(&self, now: i64, minutes: i64) -> Vec<(i64, [u32; 4])> {
        let end = now.div_euclid(60);
        (end - minutes + 1..=end)
            .map(|m| {
                (
                    m,
                    self.timeline
                        .iter()
                        .find(|t| t.0 == m)
                        .map_or([0; 4], |t| t.1),
                )
            })
            .collect()
    }

    /// The most recent flagged requests.
    pub fn recent(&self, limit: usize) -> Vec<Evidence> {
        self.events
            .iter()
            .rev()
            .take(limit)
            .map(Evidence::of)
            .collect()
    }

    /// Every detector, deterministically, at `now`.
    pub fn findings(&self, now: i64) -> Vec<SecFinding> {
        let since = now - WINDOW_SECS;
        let mut out = Vec::new();
        // The median address's requests in the last minute (the baseline).
        let mut rates: Vec<u32> = self
            .ips
            .values()
            .map(|s| s.requests(now - 60))
            .filter(|n| *n > 0)
            .collect();
        rates.sort_unstable();
        let median = rates.get(rates.len() / 2).copied().unwrap_or(0);
        for (hash, s) in &self.ips {
            if s.last < since {
                continue;
            }
            let ip = s.label.as_str();
            let n404 = s.count(sig::NOT_FOUND, since);
            let probes = s.count(sig::PROBE, since);
            let scanner = s.count(sig::SCANNER, since);
            if probes >= PROBE_HITS || n404 >= SCAN_404S || scanner > 0 {
                let exposed: Vec<Evidence> = self
                    .evidence(*hash, sig::PROBE, 200)
                    .into_iter()
                    .filter(|e| e.status < 400)
                    .collect();
                let scanner_name = s
                    .uas
                    .iter()
                    .map(|(_, f)| *f)
                    .find(|f| SCANNER_AGENTS.contains(f));
                let mut f = SecFinding::new(
                    "scanner",
                    if exposed.is_empty() {
                        Severity::Medium
                    } else {
                        Severity::High
                    },
                    format!(
                        "Scanner from {ip}: {}, {}{}",
                        super::web::count(probes, "probe", "probes"),
                        super::web::count(n404, "404", "404s"),
                        scanner_name.map(|n| format!(" ({n})")).unwrap_or_default()
                    ),
                    *hash,
                    ip,
                );
                f.count = probes.max(n404);
                f.evidence = self.evidence(*hash, sig::PROBE | sig::NOT_FOUND | sig::SCANNER, 8);
                f.detail = format!(
                    "{} of its requests asked for paths no visitor of this app asks for (/.env, /wp-login.php, /.git/config…) or got a 404; {}",
                    probes + n404,
                    if exposed.is_empty() {
                        "every probe got a 404, so nothing it looked for exists here".to_owned()
                    } else {
                        format!(
                            "{} answered below 400: {}",
                            super::web::count(exposed.len(), "probe path", "probe paths"),
                            exposed
                                .iter()
                                .map(|e| e.path.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                );
                f.action = if exposed.is_empty() {
                    "Nothing to fix in the app. In production, block the address (or the scanner's user agent) at the proxy or a WAF rule, and keep secrets out of the web root so a probe can never find them.".into()
                } else {
                    "A probe found something: make sure that path serves nothing private (no .env, .git or backups under static/), then rotate whatever it could have read.".into()
                };
                if !exposed.is_empty() {
                    f.evidence.splice(0..0, exposed.into_iter().take(3));
                    f.evidence.truncate(8);
                }
                out.push(f);
            }
            for (flag, kind, label) in [
                (sig::TRAVERSAL, "traversal", "Path traversal"),
                (sig::SQLI, "sqli", "SQL injection probes"),
                (sig::XSS, "xss", "XSS probes"),
            ] {
                let n = s.count(flag, since);
                if n >= INJECTION_HITS {
                    let ev = self.evidence(*hash, flag, 8);
                    let answered = ev.iter().filter(|e| e.status < 400).count();
                    let mut f = SecFinding::new(
                        kind,
                        Severity::Medium,
                        format!(
                            "{label} from {ip}: {n} request{}",
                            if n == 1 { "" } else { "s" }
                        ),
                        *hash,
                        ip,
                    );
                    f.count = n;
                    f.detail = format!(
                        "{} carried {} patterns in the path or query string (kept as the pattern only; the query string itself is never stored); {answered} got a 2xx or 3xx, most likely a page that ignored the parameter (check that it did).",
                        super::web::count(n, "request", "requests"),
                        label.to_ascii_lowercase()
                    );
                    f.action = match kind {
                        "traversal" => "SeaORM, Askama and ServeDir already refuse this, so the probes failed. Keep it that way: never build a filesystem path from a parameter without checking its Path::components(), and never serve files outside static/.",
                        "sqli" => "Typed SeaORM queries bind every value, so this can't reach the database as SQL. Check any raw SQL (Statement::from_string) for string interpolation; in production a WAF rule can drop these early.",
                        _ => "Askama escapes every value and the CSP blocks inline script, so a reflected payload can't run. Never use the safe filter on user input; watch the CSP report count.",
                    }
                    .into();
                    f.evidence = ev;
                    out.push(f);
                }
            }
            let odd = s.count(sig::ODD_METHOD, since);
            if odd > 0 {
                let mut f = SecFinding::new(
                    "odd_method",
                    Severity::Low,
                    format!("Unusual methods from {ip}: {odd} (TRACE, PROPFIND…)"),
                    *hash,
                    ip,
                );
                f.count = odd;
                f.detail = "Methods no browser sends: a scanner fingerprinting the server.".into();
                f.action = "The router answers 405; nothing to fix. A proxy can drop them before they reach the app.".into();
                f.evidence = self.evidence(*hash, sig::ODD_METHOD, 8);
                out.push(f);
            }
            let big = s.count(sig::BIG_BODY | sig::BIG_HEADERS, since);
            if big > 0 {
                let mut f = SecFinding::new(
                    "oversized",
                    Severity::Low,
                    format!("Oversized requests from {ip}: {big}"),
                    *hash,
                    ip,
                );
                f.count = big;
                f.detail = format!(
                    "Bodies past {} or headers past {} KB: a denial-of-service attempt or a misbehaving client.",
                    super::web::bytes(BIG_BODY),
                    BIG_HEADERS / 1024
                );
                f.action = "The body limit (DefaultBodyLimit) answered 413; keep it. A proxy limit in front stops them earlier.".into();
                f.evidence = self.evidence(*hash, sig::BIG_BODY | sig::BIG_HEADERS, 8);
                out.push(f);
            }
            let bad_host = s.count(sig::BAD_HOST, since);
            if bad_host > 0 {
                let mut f = SecFinding::new(
                    "bad_host",
                    Severity::Medium,
                    format!("Requests for a host this app doesn't serve from {ip}: {bad_host}"),
                    *hash,
                    ip,
                );
                f.count = bad_host;
                f.detail = "The Host allowlist refused them (403): in development that is what a DNS-rebinding page looks like, or a tunnel that isn't in APP_ALLOWED_HOSTS yet.".into();
                f.action = "If you opened the app through a tunnel or a LAN name, add it to APP_ALLOWED_HOSTS; otherwise the check did its job.".into();
                f.evidence = self.evidence(*hash, sig::BAD_HOST, 8);
                out.push(f);
            }
            let csrf = s.count(sig::CSRF, since);
            if csrf >= CSRF_HITS {
                let mut f = SecFinding::new(
                    "csrf",
                    Severity::Medium,
                    format!("Cross-site POSTs from {ip}: {csrf} rejected by CSRF"),
                    *hash,
                    ip,
                );
                f.count = csrf;
                f.detail = "Unsafe requests sent from another site (Sec-Fetch-Site: cross-site or a foreign Origin), refused with 403 by the CSRF layer.".into();
                f.action = "The CSRF layer held. If one of your own pages did this, it posts to the wrong origin (check APP_URL); otherwise someone is trying to make your users' browsers act for them.".into();
                f.evidence = self.evidence(*hash, sig::CSRF, 8);
                out.push(f);
            }
            let limited = s.count(sig::RATE_LIMITED, since);
            if limited >= RATE_LIMITED {
                let mut f = SecFinding::new(
                    "rate_limited",
                    Severity::Medium,
                    format!("{ip} keeps hitting rate limits: {limited} × 429"),
                    *hash,
                    ip,
                );
                f.count = limited;
                f.detail = "Its requests were refused with 429 Too Many Requests: the limits are working, and it keeps going.".into();
                f.action = "Nothing to fix in the app; in production, a proxy or WAF rule for this address saves the app the work.".into();
                f.evidence = self.evidence(*hash, sig::RATE_LIMITED, 8);
                out.push(f);
            }
            // Credential stuffing: one address, many accounts (with the auth hook), or many failed
            // sign-ins (without it).
            let failures = (s.auth_failed.iter().filter(|t| **t >= since).count() as u32)
                .max(s.count(sig::SIGN_IN_FAILED, since));
            if s.accounts.len() as u32 >= STUFFING_ACCOUNTS
                || (s.accounts.is_empty() && failures >= STUFFING_FAILURES)
            {
                let mut f = SecFinding::new(
                    "credential_stuffing",
                    Severity::High,
                    if s.accounts.is_empty() {
                        format!("Credential stuffing from {ip}: {failures} failed sign-ins")
                    } else {
                        format!(
                            "Credential stuffing from {ip}: {} accounts tried, {failures} failed sign-ins",
                            s.accounts.len()
                        )
                    },
                    *hash,
                    ip,
                );
                f.count = failures;
                f.detail = format!(
                    "One address signing in to {} with passwords that don't work: a list of stolen credentials being tried.{}",
                    if s.accounts.is_empty() {
                        "accounts the dashboard can't tell apart (no auth hook)".to_owned()
                    } else {
                        format!("{} different accounts", s.accounts.len())
                    },
                    if s.signed_in_after_failures > 0 {
                        format!(
                            " {} sign-in(s) from it then succeeded: check those accounts now.",
                            s.signed_in_after_failures
                        )
                    } else {
                        String::new()
                    }
                );
                f.action = "The per-account throttle slows each account, not the address: add an IP-level limit on sign-in (auth-password's IP_LIMIT, or a WAF rule), and turn on breached-password checks and two-factor (auth-totp).".into();
                if s.signed_in_after_failures > 0 {
                    f.severity = Severity::High;
                }
                f.evidence = self.evidence(*hash, sig::SIGN_IN_FAILED, 8);
                out.push(f);
            }
            // WHY: this machine is the developer, the load lab and the screenshot browser: its
            // volume and variety say nothing (probes and injections from it still count).
            if s.local {
                continue;
            }
            let recent = s.requests(now - 60);
            if recent >= RATE_FLOOR && recent >= RATE_FACTOR * median.max(1) {
                let mut f = SecFinding::new(
                    "ip_rate",
                    Severity::Medium,
                    format!(
                        "{ip} sent {recent} requests in the last minute ({}× the median address)",
                        recent / median.max(1)
                    ),
                    *hash,
                    ip,
                );
                f.count = recent;
                f.detail = format!("The median active address sent {median} in the same minute.");
                f.action = "A load test, a crawler or a scraper. If it isn't yours, rate-limit it at the proxy (per address) and cache what it reads.".into();
                f.evidence = self.evidence(*hash, u32::MAX, 5);
                out.push(f);
            }
            let total = s.requests(since).max(1);
            let errors = s.count(sig::CLIENT_ERROR, since);
            if errors >= IP_4XX && errors * 2 >= total {
                let mut f = SecFinding::new(
                    "ip_4xx",
                    Severity::Low,
                    format!("{ip} gets mostly errors: {errors} 4xx of {total} requests"),
                    *hash,
                    ip,
                );
                f.count = errors;
                f.detail = "An address whose requests mostly fail is guessing (URLs, ids, credentials) rather than browsing.".into();
                f.action = "Look at what it asks for (the evidence); if it is guessing ids, make sure every lookup checks ownership (a 404 for someone else's record).".into();
                f.evidence = self.evidence(*hash, sig::CLIENT_ERROR, 8);
                out.push(f);
            }
            if s.uas.len() >= UA_ROTATION {
                let mut fams: Vec<&str> = s.uas.iter().map(|(_, f)| *f).collect();
                fams.sort_unstable();
                fams.dedup();
                let mut f = SecFinding::new(
                    "ua_rotation",
                    Severity::Low,
                    format!("{ip} used {} different user agents", s.uas.len()),
                    *hash,
                    ip,
                );
                f.count = s.uas.len() as u32;
                f.detail = format!(
                    "Families: {}. Browsers don't change their user agent; scripts dodging a filter do (behind a shared NAT this can also be an office).",
                    fams.join(", ")
                );
                f.action = "Don't block on the user agent alone; rate-limit by address and watch its other findings.".into();
                out.push(f);
            }
            let spans = s.sessions.len().max(s.accounts.len());
            if spans >= IP_SESSIONS {
                let mut f = SecFinding::new(
                    "ip_sessions",
                    Severity::Low,
                    format!("{ip} spans {} sessions", s.sessions.len()),
                    *hash,
                    ip,
                );
                f.count = spans as u32;
                f.detail = format!(
                    "{} sessions and {} accounts from one address: an office behind one NAT, or one client cycling sessions.",
                    s.sessions.len(),
                    s.accounts.len()
                );
                f.action = "Usually fine (offices, mobile carriers). Worth a look together with failed sign-ins from the same address.".into();
                out.push(f);
            }
        }
        for (acct, a) in &self.accounts {
            let n = a.failed.iter().filter(|t| **t >= since).count() as u32;
            if n >= BRUTE_FAILURES {
                let ips: Vec<&str> = a.ips.iter().map(|(_, l)| l.as_str()).take(5).collect();
                let mut f = SecFinding::new(
                    "brute_force",
                    if a.throttled > 0 {
                        Severity::Medium
                    } else {
                        Severity::High
                    },
                    format!(
                        "Brute force on one account: {n} failed sign-ins in {} min",
                        WINDOW_SECS / 60
                    ),
                    *acct,
                    ips.first().copied().unwrap_or("unknown"),
                );
                f.count = n;
                f.detail = format!(
                    "From {} address(es): {}. {}",
                    a.ips.len(),
                    ips.join(", "),
                    if a.throttled > 0 {
                        "The per-account throttle held (the account was paused, its owner told)."
                    } else {
                        "No throttle answered yet."
                    }
                );
                f.action = if a.throttled > 0 {
                    "The throttle held. If it continues from many addresses, it is distributed: consider a CAPTCHA after N failures and two-factor for this account.".into()
                } else {
                    "Make sure sign-in is throttled per account (auth-password's progressive delay) and per address.".into()
                };
                out.push(f);
            }
        }
        for (sess, st) in &self.sessions {
            let ips = st.ips.iter().filter(|(_, _, t)| *t >= since).count();
            let uas = st.uas.iter().filter(|(_, t)| *t >= since).count();
            if ips >= SESSION_IPS || uas >= SESSION_IPS {
                let labels: Vec<&str> = st.ips.iter().map(|(_, l, _)| l.as_str()).take(5).collect();
                let mut f = SecFinding::new(
                    "session_hop",
                    Severity::High,
                    format!("One session used from {ips} addresses and {uas} user agents"),
                    *sess,
                    labels.first().copied().unwrap_or("unknown"),
                );
                f.count = ips.max(uas) as u32;
                f.detail = format!(
                    "Within {} minutes, from {}: a stolen session cookie being replayed (or a phone switching networks).",
                    WINDOW_SECS / 60,
                    labels.join(", ")
                );
                f.action = "If it isn't a phone roaming, revoke the session (auth-core's account page lists sessions) and look at how the cookie could have leaked. Binding sessions to a device cookie limits replay.".into();
                out.push(f);
            }
        }
        let spikes: Vec<(i64, u32)> = self
            .csp
            .iter()
            .filter(|(m, n)| m * 60 >= since && *n >= CSP_SPIKE)
            .copied()
            .collect();
        if let Some((m, n)) = spikes.iter().max_by_key(|(_, n)| *n) {
            let mut f = SecFinding::new(
                "csp_spike",
                Severity::High,
                format!("CSP violation spike: {n} reports in one minute"),
                0,
                "browsers",
            );
            f.count = *n;
            f.detail = format!(
                "At {}: an injected script being blocked, a browser extension, or a deploy that broke the policy (Insights lists each violation).",
                DateTime::from_timestamp(m * 60, 0)
                    .map_or_else(String::new, |t| t.format("%H:%M UTC").to_string())
            );
            f.action = "Open the CSP violations panel: if the blocked source is yours, move it into static/; if it isn't, the policy just stopped an attack.".into();
            out.push(f);
        }
        out.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then(b.count.cmp(&a.count))
                .then(a.title.cmp(&b.title))
        });
        out
    }
}

/// A flagged request, as evidence.
#[derive(Clone, Debug)]
pub struct Evidence {
    pub at: i64,
    pub ip: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub labels: Vec<&'static str>,
    pub ua: &'static str,
    pub trace: u64,
}

impl Evidence {
    fn of(e: &Obs) -> Self {
        Self {
            at: e.at,
            ip: e.ip_label.clone(),
            method: e.method.clone(),
            path: e.path.clone(),
            status: e.status,
            labels: sig::labels(e.flags & !sig::CLIENT_ERROR),
            ua: e.ua,
            trace: e.trace,
        }
    }

    pub fn when(&self) -> String {
        DateTime::from_timestamp(self.at, 0)
            .map_or_else(String::new, |t| t.format("%H:%M:%S").to_string())
    }
}

/// One security finding.
#[derive(Clone, Debug)]
pub struct SecFinding {
    pub kind: &'static str,
    pub severity: Severity,
    pub title: String,
    /// The subject's keyed hash (an address, an account, a session).
    pub subject: u64,
    /// The address (as shown) it is about.
    pub ip: String,
    pub count: u32,
    pub detail: String,
    pub action: String,
    pub evidence: Vec<Evidence>,
}

impl SecFinding {
    fn new(kind: &'static str, severity: Severity, title: String, subject: u64, ip: &str) -> Self {
        Self {
            kind,
            severity,
            title,
            subject,
            ip: ip.to_owned(),
            count: 0,
            detail: String::new(),
            action: String::new(),
            evidence: Vec::new(),
        }
    }

    /// Stable across refreshes (an 8-hex id for links and Ask AI).
    pub fn id(&self) -> String {
        format!("{:08x}", keyed((self.kind, self.subject)) & 0xffff_ffff)
    }
}

/// Security findings as Insights (ranked with the others).
pub fn findings() -> Vec<Finding> {
    let list = lock(&HUB.threats).findings(Utc::now().timestamp());
    list.into_iter()
        .map(|f| Finding {
            key: format!("sec-{}-{:x}", f.kind, f.subject),
            kind: "security",
            severity: f.severity,
            score: u64::from(f.count),
            title: f.title.clone(),
            evidence: f.detail.clone(),
            fix: f.action.clone(),
            guide: ".offrails/guides/security.md",
            guide_url: "https://rustoffrails.org/docs/guides/security",
            href: format!("/dev/dashboard/security#sec-{}", f.id()),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------------------------

pub struct Bar {
    pub x: String,
    pub w: String,
    pub parts: Vec<(String, String, &'static str)>,
    pub title: String,
}

pub struct IpRow {
    pub label: String,
    pub requests: u32,
    pub errors: u32,
    pub probes: u32,
    pub injections: u32,
    pub auth: u32,
    pub uas: usize,
    pub last: String,
    pub tone: &'static str,
}

#[derive(Template)]
#[template(path = "kernel/dashboard/security.html")]
struct SecurityPage {
    chrome: ui::Chrome,
    d: Dash,
    base: &'static str,
    verdict: &'static str,
    verdict_tone: &'static str,
    summary: String,
    bars: Vec<Bar>,
    ticks: Vec<(String, String)>,
    peak: u32,
    totals: [u32; 4],
    ips: Vec<IpRow>,
    findings: Vec<(String, SecFinding)>,
    recent: Vec<Evidence>,
    requests: u64,
    production: bool,
}

pub const TIMELINE_MINUTES: i64 = 30;

/// `GET …/security`: the verdict, the timeline, the busiest addresses, the findings with their
/// evidence, the latest flagged requests.
pub async fn security_page(
    State(state): State<AppState>,
    Extension(mode): Extension<Mode>,
    theme: ThemeSwitch,
    flash: Option<Extension<RequestFlash>>,
) -> Response {
    let now = Utc::now().timestamp();
    let (findings, timeline, mut ips, recent, requests) = {
        let t = lock(&HUB.threats);
        let since = now - WINDOW_SECS;
        let ips: Vec<IpRow> = t
            .ips
            .values()
            .filter(|s| {
                s.last >= since
                    && (s.hits.iter().any(|(_, f)| f & sig::NOTABLE != 0)
                        || s.auth_failed.len() as u32 >= 3)
            })
            .map(|s| {
                let probes = s.count(sig::PROBE | sig::SCANNER | sig::ODD_METHOD, since);
                let injections = s.count(sig::TRAVERSAL | sig::SQLI | sig::XSS, since);
                let auth = s.count(sig::SIGN_IN_FAILED, since) + s.auth_failed.len() as u32;
                IpRow {
                    label: s.label.clone(),
                    requests: s.requests(since),
                    errors: s.count(sig::CLIENT_ERROR, since),
                    probes,
                    injections,
                    auth,
                    uas: s.uas.len(),
                    last: ui::ago(
                        DateTime::from_timestamp(s.last, 0).unwrap_or_default(),
                        Utc::now(),
                    ),
                    tone: if injections > 0 || auth >= STUFFING_FAILURES {
                        "err"
                    } else {
                        "warn"
                    },
                }
            })
            .collect();
        (
            t.findings(now),
            t.timeline(now, TIMELINE_MINUTES),
            ips,
            t.recent(40),
            t.requests,
        )
    };
    ips.sort_by_key(|r| {
        std::cmp::Reverse(r.probes * 10 + r.injections * 20 + r.auth * 5 + r.errors)
    });
    ips.truncate(15);
    let peak = timeline
        .iter()
        .map(|(_, c)| c.iter().sum::<u32>())
        .max()
        .unwrap_or(0);
    let scale = super::charts::nice_ceiling(f64::from(peak)).max(1.0);
    let n = timeline.len().max(1) as f64;
    let mut totals = [0_u32; 4];
    let bars: Vec<Bar> = timeline
        .iter()
        .enumerate()
        .map(|(i, (m, c))| {
            let mut y = 100.0;
            let parts = c
                .iter()
                .zip(CATEGORIES)
                .enumerate()
                .filter(|(_, (v, _))| **v > 0)
                .map(|(k, (v, cat))| {
                    totals[k] += v;
                    let h = f64::from(*v) / scale * 96.0;
                    y -= h;
                    (format!("{y:.2}%"), format!("{h:.2}%"), cat)
                })
                .collect();
            Bar {
                x: format!("{:.2}%", i as f64 / n * 100.0 + 0.6),
                w: format!("{:.2}%", 100.0 / n - 1.2),
                parts,
                title: format!(
                    "{}: {} probes, {} injection, {} auth, {} abuse",
                    DateTime::from_timestamp(m * 60, 0)
                        .map_or_else(String::new, |t| t.format("%H:%M").to_string()),
                    c[0],
                    c[1],
                    c[2],
                    c[3]
                ),
            }
        })
        .collect();
    let ticks = [0, TIMELINE_MINUTES / 2, TIMELINE_MINUTES - 1]
        .iter()
        .filter_map(|i| timeline.get(*i as usize).map(|(m, _)| (*i, *m)))
        .map(|(i, m)| {
            (
                format!("{:.1}%", i as f64 / n * 100.0 + 50.0 / n),
                DateTime::from_timestamp(m * 60, 0)
                    .map_or_else(String::new, |t| t.format("%H:%M").to_string()),
            )
        })
        .collect();
    let high = findings
        .iter()
        .filter(|f| f.severity == Severity::High)
        .count();
    let (verdict, verdict_tone) = match (high, findings.len()) {
        (0, 0) => ("Quiet", "ok"),
        (0, _) => ("Watch", "warn"),
        _ => ("Under attack", "err"),
    };
    let summary = if findings.is_empty() {
        format!(
            "Nothing suspicious in the last {} minutes of {} requests.",
            WINDOW_SECS / 60,
            requests
        )
    } else {
        format!(
            "{} finding{} ({high} high) in the last {} minutes; the detectors look at every request and sign-in event.",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" },
            WINDOW_SECS / 60
        )
    };
    ui::page(SecurityPage {
        chrome: chrome(&state, "Security", "dashboard", flash, theme),
        d: dash(mode, "/security", None, 0).ai(&state.config, ai::Tab::Security, None),
        base: mode.base,
        verdict,
        verdict_tone,
        summary,
        bars,
        ticks,
        peak,
        totals,
        ips,
        findings: findings.into_iter().map(|f| (f.id(), f)).collect(),
        recent,
        requests,
        production: state.config.is_production(),
    })
}

// ---------------------------------------------------------------------------------------------
// For Ask AI (addresses as shown on the page: truncated and hashed in production)
// ---------------------------------------------------------------------------------------------

/// `list_security_findings`: every finding with its evidence ids.
pub fn list_text(_ro: bool) -> String {
    let t = lock(&HUB.threats);
    let list = t.findings(Utc::now().timestamp());
    if list.is_empty() {
        return format!(
            "No security findings in the last {} minutes ({} requests seen). The load lab's attack simulator plants some.",
            WINDOW_SECS / 60,
            t.requests
        );
    }
    let mut out = String::from(
        "id kind severity: title. detail. action. evidence (event ids for get_security_event)\n",
    );
    for f in list.iter().take(20) {
        let ev: Vec<String> = f
            .evidence
            .iter()
            .take(4)
            .map(|e| {
                let id = event_id(e);
                if e.trace > 0 {
                    format!(
                        "{id} ({} {} → {}, [trace:{}])",
                        e.method, e.path, e.status, e.trace
                    )
                } else {
                    format!("{id} ({} {} → {})", e.method, e.path, e.status)
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "- {} {} {}: {}. {} {} evidence: {}",
            f.id(),
            f.kind,
            f.severity.key(),
            ai_clean(&f.title),
            ai_clean(&f.detail),
            f.action,
            if ev.is_empty() {
                "(aggregate)".to_owned()
            } else {
                ev.join(", ")
            }
        );
    }
    out
}

fn ai_clean(s: &str) -> String {
    super::ai_context::clean(s, 400)
}

/// An event's id: 12 hex of its time, address and path.
pub fn event_id(e: &Evidence) -> String {
    format!(
        "{:012x}",
        keyed((e.at, &e.ip, &e.path, e.status)) & 0xffff_ffff_ffff
    )
}

/// `get_security_event`: one flagged request by its id.
pub fn event_text(id: &str) -> Result<String, String> {
    let t = lock(&HUB.threats);
    let found = t
        .events
        .iter()
        .rev()
        .map(Evidence::of)
        .find(|e| event_id(e) == id);
    match found {
        Some(e) => Ok(format!(
            "{} at {} UTC from {} (user agent family: {}): {} {} → {}; signals: {}{}",
            event_id(&e),
            e.when(),
            e.ip,
            e.ua,
            e.method,
            ai_clean(&e.path),
            e.status,
            e.labels.join(", "),
            if e.trace > 0 {
                format!("; its trace: [trace:{}]", e.trace)
            } else {
                String::new()
            }
        )),
        None => {
            Err("No such event (ids come from list_security_findings; old events roll off).".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_patterns_are_recognized() {
        assert_ne!(patterns("/.env", "") & sig::PROBE, 0);
        assert_ne!(patterns("/wp-login.php", "") & sig::PROBE, 0);
        assert_eq!(patterns("/environment", "") & sig::PROBE, 0);
        assert_ne!(
            patterns("/static/..%2f..%2fetc/passwd", "") & sig::TRAVERSAL,
            0
        );
        assert_ne!(
            patterns("/x", "file=%252e%252e%252fetc") & sig::TRAVERSAL,
            0
        );
        assert_ne!(
            patterns("/x", "id=1%27%20OR%20%271%27%3D%271") & sig::SQLI,
            0
        );
        assert_ne!(
            patterns("/x", "q=%3Cscript%3Ealert(1)%3C/script%3E") & sig::XSS,
            0
        );
        assert_eq!(patterns("/orders", "page=2&sort=date"), 0);
    }
}
