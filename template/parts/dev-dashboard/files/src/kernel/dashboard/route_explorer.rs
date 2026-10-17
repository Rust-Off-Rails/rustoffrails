//! The Routes explorer: every route in `ROUTES`, grouped by area and resource, with its handler's
//! source, access level and middleware, live numbers from the request collector, and the problems
//! worth fixing: errors, slowness, routes never hit, routes no test mentions, mutating routes
//! without CSRF, admin or dev paths without a gate, GETs that write, 404s with the nearest route,
//! overlapping patterns, and routes missing from `docs/routes.md`.
//!
//! The detectors take their inputs as arguments (a router, a route table, source text), so each is
//! tested against an app with that bug planted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::routes::{Access, ROUTES};

/// A route as the explorer sees it (`ROUTES` rows, or a planted table in tests).
#[derive(Clone, Debug)]
pub struct RouteDef {
    pub method: &'static str,
    pub path: &'static str,
    pub handler: &'static str,
    pub access: Access,
}

pub fn table() -> Vec<RouteDef> {
    ROUTES
        .iter()
        .map(|(method, path, handler, access)| RouteDef {
            method,
            path,
            handler,
            access: *access,
        })
        .collect()
}

/// `app`, `api`, `admin`, `dev` or `static`.
pub fn area(path: &str) -> &'static str {
    if path.starts_with("/dev") {
        "dev"
    } else if path.starts_with("/admin") {
        "admin"
    } else if path.starts_with("/api") {
        "api"
    } else {
        "app"
    }
}

/// The resource a route belongs to: the first path segment after the area prefix
/// (`/dev/mailbox/{id}` → `mailbox`, `/books/{id}/edit` → `books`, `/` → `home`).
pub fn resource(path: &str) -> String {
    let segs: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect();
    let skip = usize::from(matches!(segs.first(), Some(&("dev" | "admin" | "api"))));
    let seg = segs.get(skip).or(segs.first()).copied().unwrap_or("home");
    seg.split(['.', '?']).next().unwrap_or(seg).to_owned()
}

pub fn access_label(a: Access) -> &'static str {
    match a {
        Access::Public => "public",
        Access::User => "login",
        Access::Admin => "admin",
        Access::Dev => "dev",
    }
}

/// A sample URL for a pattern: `{id}` → `1`, any other parameter → `example`.
pub fn sample_url(path: &str) -> String {
    path.split('/')
        .map(|s| {
            if s == "{id}" || s == "{n}" {
                "1".to_owned()
            } else if s.starts_with('{') {
                "example".to_owned()
            } else {
                s.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a handler lives: `home::index` → `src/controllers/home.rs:6`, relative to the app.
// SECURITY: app-relative paths only; the absolute path of the checkout never reaches the page
// (test: route_sources_are_app_relative).
pub fn source_of(root: &Path, handler: &str) -> Option<String> {
    let (module, func) = handler.split_once("::")?;
    if !module
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let candidates = [
        format!("src/controllers/{module}.rs"),
        format!("src/controllers/{module}/mod.rs"),
        format!("src/kernel/{module}.rs"),
        format!("src/kernel/{module}/mod.rs"),
        format!("src/kernel/dashboard/{module}.rs"),
        format!("src/{module}.rs"),
    ];
    for rel in candidates {
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let needle = format!("fn {func}(");
        let line = text
            .lines()
            .position(|l| l.contains(&needle))
            .map(|i| i + 1);
        return Some(match line {
            Some(n) => format!("{rel}:{n}"),
            None => rel,
        });
    }
    None
}

/// What guards a route, from its access level and its handler's source.
pub fn middleware(def: &RouteDef, handler_source: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !matches!(def.method, "GET" | "HEAD" | "OPTIONS") {
        out.push("CSRF");
    }
    if handler_source.contains("rate_limit") {
        out.push("rate limit");
    }
    match def.access {
        Access::Admin => out.push("admin gate"),
        Access::User => out.push("login"),
        Access::Dev => out.push("dev only"),
        Access::Public => {}
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Detectors
// ---------------------------------------------------------------------------------------------

/// A method none of the routes on `path` accepts, so a probe can never run a handler.
fn unused_method(routes: &[RouteDef], path: &str) -> Method {
    let taken: Vec<&str> = routes
        .iter()
        .filter(|r| r.path == path)
        .map(|r| r.method)
        .collect();
    [Method::PATCH, Method::PUT, Method::DELETE, Method::POST]
        .into_iter()
        .find(|m| !taken.contains(&m.as_str()))
        .unwrap_or(Method::PATCH)
}

/// Mutating routes that accept a cross-site request: probed with a cross-site request in a method
/// the route doesn't have. The CSRF layer answers 403 before routing; without it the router
/// answers 405, and no handler ever runs.
// SECURITY: the probe never reaches a handler (an unused method), so checking can't change data
// (test: a_mutating_route_without_csrf_is_spotted).
pub async fn without_csrf(router: &Router, routes: &[RouteDef]) -> Vec<String> {
    let mut out = Vec::new();
    for r in routes
        .iter()
        .filter(|r| !matches!(r.method, "GET" | "HEAD" | "OPTIONS"))
    {
        let method = unused_method(routes, r.path);
        let Ok(req) = Request::builder()
            .method(method)
            .uri(sample_url(r.path))
            .header("sec-fetch-site", "cross-site")
            .header("origin", "https://csrf-probe.invalid")
            .body(Body::empty())
        else {
            continue;
        };
        if let Ok(res) = router.clone().oneshot(req).await
            && res.status() != StatusCode::FORBIDDEN
            && !out.contains(&format!("{} {}", r.method, r.path))
        {
            out.push(format!("{} {}", r.method, r.path));
        }
    }
    out
}

/// `/admin` or `/dev` paths whose access level says anyone may call them.
pub fn ungated(routes: &[RouteDef]) -> Vec<String> {
    routes
        .iter()
        .filter(|r| {
            (r.path.starts_with("/admin") && !matches!(r.access, Access::Admin))
                || (r.path.starts_with("/dev") && !matches!(r.access, Access::Dev))
        })
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

/// Admin and login routes that answer 2xx anonymously on `router` (build it with the production
/// config: development opens admin pages on purpose). GETs only.
pub async fn open_to_anyone(router: &Router, routes: &[RouteDef]) -> Vec<String> {
    let mut out = Vec::new();
    for r in routes
        .iter()
        .filter(|r| r.method == "GET" && matches!(r.access, Access::Admin | Access::User))
    {
        let Ok(req) = Request::get(sample_url(r.path)).body(Body::empty()) else {
            continue;
        };
        if let Ok(res) = router.clone().oneshot(req).await
            && res.status().is_success()
        {
            out.push(format!("{} {}", r.method, r.path));
        }
    }
    out
}

/// Pairs of patterns a request can match both of (same method): `/posts/{id}` and `/posts/new`.
/// axum prefers the literal one; a reader may not.
pub fn overlaps(routes: &[RouteDef]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in routes.iter().enumerate() {
        for b in routes.iter().skip(i + 1) {
            if a.method != b.method || a.path == b.path {
                continue;
            }
            let sa: Vec<&str> = a.path.split('/').collect();
            let sb: Vec<&str> = b.path.split('/').collect();
            if sa.len() == sb.len()
                && sa
                    .iter()
                    .zip(&sb)
                    .all(|(x, y)| x == y || x.starts_with('{') || y.starts_with('{'))
            {
                out.push((
                    format!("{} {}", a.method, a.path),
                    format!("{} {}", b.method, b.path),
                ));
            }
        }
    }
    out
}

/// Routes `docs/routes.md` doesn't list (it is regenerated by `app routes`).
pub fn missing_from_docs(routes: &[RouteDef], doc: &str) -> Vec<String> {
    routes
        .iter()
        .filter(|r| !doc.contains(&format!("| {} | `{}` |", r.method, r.path)))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

/// APP: the rule that decides a test covers a route. The default: some file under `tests/`
/// mentions the route's path up to its first parameter (`"/dev/mailbox/"` for
/// `/dev/mailbox/{id}`), or the whole path. The route sweeps in `security_test.rs` request every
/// route generically and don't count.
pub fn tested(path: &str, tests: &str) -> bool {
    let literal = path.split('{').next().unwrap_or(path);
    let literal = if literal.len() > 1 { literal } else { path };
    tests.contains(&format!("\"{path}"))
        || (literal != "/" && tests.contains(&format!("\"{literal}")))
        || (path == "/" && (tests.contains("get(\"/\")") || tests.contains("(\"/\")")))
}

/// Every test file's text (at most 2 MB).
pub fn tests_text(root: &Path) -> String {
    let mut out = String::new();
    let mut stack: Vec<PathBuf> = vec![root.join("tests")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p.ends_with("security_test.rs")
                && let Ok(t) = std::fs::read_to_string(&p)
            {
                out.push_str(&t);
                if out.len() > 2 << 20 {
                    return out;
                }
            }
        }
    }
    out
}

/// Levenshtein distance (for "did you mean").
pub fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The route a mistyped path most likely meant (a pattern's parameters match any segment), when
/// it's close enough to be a typo.
pub fn nearest(path: &str, routes: &[RouteDef]) -> Option<&'static str> {
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    routes
        .iter()
        .filter(|r| r.method == "GET")
        .map(|r| {
            let ps: Vec<&str> = r.path.trim_end_matches('/').split('/').collect();
            // Replace parameters with what the request had there, then compare the strings.
            let filled: Vec<&str> = ps
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if p.starts_with('{') {
                        segs.get(i).copied().unwrap_or(p)
                    } else {
                        p
                    }
                })
                .collect();
            (
                distance(path.trim_end_matches('/'), &filled.join("/")),
                r.path,
            )
        })
        .filter(|(d, p)| *d > 0 && *d <= (p.len() / 4).clamp(1, 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, p)| p)
}

/// The groups: area → resource → routes, in table order.
pub fn grouped(routes: &[RouteDef]) -> Vec<((&'static str, String), Vec<RouteDef>)> {
    let rank = |a: &str| match a {
        "app" => 0,
        "api" => 1,
        "admin" => 2,
        _ => 3,
    };
    let mut out: BTreeMap<(u8, &'static str, String), Vec<RouteDef>> = BTreeMap::new();
    for r in routes {
        let a = area(r.path);
        out.entry((rank(a), a, resource(r.path)))
            .or_default()
            .push(r.clone());
    }
    out.into_iter()
        .map(|((_, a, res), v)| ((a, res), v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_group_by_area_and_resource() {
        assert_eq!(area("/dev/mailbox/{id}"), "dev");
        assert_eq!(resource("/dev/mailbox/{id}"), "mailbox");
        assert_eq!(resource("/books/{id}/edit"), "books");
        assert_eq!(resource("/"), "home");
        assert_eq!(resource("/sitemap.xml"), "sitemap");
        assert_eq!(resource("/dev"), "dev");
        assert_eq!(
            sample_url("/dev/mailbox/{id}/attachments/{n}"),
            "/dev/mailbox/1/attachments/1"
        );
    }

    #[test]
    fn typos_find_their_route() {
        let routes = vec![
            RouteDef {
                method: "GET",
                path: "/dev/routes",
                handler: "dev::routes",
                access: Access::Dev,
            },
            RouteDef {
                method: "GET",
                path: "/dev/mailbox/{id}",
                handler: "m::show",
                access: Access::Dev,
            },
        ];
        assert_eq!(nearest("/dev/rotues", &routes), Some("/dev/routes"));
        assert_eq!(
            nearest("/dev/mailbx/12", &routes),
            Some("/dev/mailbox/{id}")
        );
        assert_eq!(nearest("/wp-login.php", &routes), None);
        assert_eq!(distance("kitten", "sitting"), 3);
    }
}
