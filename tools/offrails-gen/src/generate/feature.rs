//! A feature's routes file, `src/routes/<feature>.rs`: its router, its `paths::` helpers and its
//! `ROUTES` rows, wired into `src/routes.rs` by one marked line at each of four anchors
//! (`route mods`, `routes`, `paths`, `route tables`). Removing a feature is deleting the file and
//! those four lines.

use super::app::AppInfo;
use super::plan::Plan;
use super::routes_md;
use crate::util::{Fail, Result};

/// One `.route(path, …)` line: the path and its (method, handler path) pairs.
pub struct Mount {
    pub path: String,
    pub methods: Vec<(&'static str, String)>,
}

impl Mount {
    pub fn new(path: impl Into<String>, methods: &[(&'static str, &str)]) -> Self {
        Mount {
            path: path.into(),
            methods: methods.iter().map(|(m, h)| (*m, (*h).to_owned())).collect(),
        }
    }
}

/// What goes into `src/routes/<module>.rs`.
pub struct Routes {
    /// The module (and file) name: `posts`, `api_v1_posts`, `about`.
    pub module: String,
    /// Who wrote it, for the file's doc line.
    pub what: String,
    pub mounts: Vec<Mount>,
    /// `pub fn …` helpers, unindented (they go inside `pub mod paths { … }`).
    pub paths: String,
    /// (method, path, handler, access) rows.
    pub rows: Vec<(String, String, String, String)>,
    /// A file several generations share (the API kernel's): created once, removed with the last.
    pub shared: bool,
}

/// `("GET",  "/posts",  "posts::index",  Access::User),` lines, aligned.
pub fn table(rows: &[(String, String, String, String)]) -> String {
    let wp = rows.iter().map(|r| r.1.len() + 3).max().unwrap_or(10);
    let wh = rows.iter().map(|r| r.2.len() + 3).max().unwrap_or(10);
    rows.iter()
        .map(|(m, p, h, a)| {
            let m = format!("\"{m}\",");
            let p = format!("\"{p}\",");
            let h = format!("\"{h}\",");
            format!("    ({m:<7} {p:<wp$} {h:<wh$} Access::{a}),\n")
        })
        .collect()
}

pub fn path_of(module: &str) -> String {
    format!("src/routes/{module}.rs")
}

/// The routes file's text.
pub fn file(r: &Routes) -> String {
    // Only each chain's first verb is a `routing::` function; the rest are `MethodRouter` methods.
    let mut methods: Vec<&str> = r
        .mounts
        .iter()
        .filter_map(|m| m.methods.first().map(|(x, _)| *x))
        .collect();
    methods.sort_unstable();
    methods.dedup();
    let routing = if methods.len() == 1 {
        methods[0].to_owned()
    } else {
        format!("{{{}}}", methods.join(", "))
    };
    let mut router = String::from("    Router::new()\n");
    for m in &r.mounts {
        let chain: Vec<String> = m
            .methods
            .iter()
            .enumerate()
            .map(|(i, (verb, h))| {
                if i == 0 {
                    format!("{verb}({h})")
                } else {
                    format!(".{verb}({h})")
                }
            })
            .collect();
        router.push_str(&format!(
            "        .route(\"{}\", {})\n",
            m.path,
            chain.join("")
        ));
    }
    let paths: String = r
        .paths
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                "\n".to_owned()
            } else {
                format!("    {l}\n")
            }
        })
        .collect();
    format!(
        "//! {what}: its router, `paths::` helpers and `ROUTES` rows.\n//! src/routes.rs wires it in with one `// {m}` line at `route mods`, `routes`, `paths`, `route tables`.\n\nuse axum::{{Router, routing::{routing}}};\n\nuse super::{{Access, Route}};\nuse crate::state::AppState;\n\npub fn router() -> Router<AppState> {{\n{router}}}\n\n/// URL helpers; `crate::routes::paths` re-exports them.\npub mod paths {{\n{paths}}}\n\n#[rustfmt::skip]\npub const ROUTES: &[Route] = &[\n{rows}];\n",
        what = r.what,
        m = r.module,
        rows = table(&r.rows),
    )
}

/// The routes file and its four wiring lines.
pub fn plan_into(plan: &mut Plan, r: &Routes) {
    let path = path_of(&r.module);
    let m = &r.module;
    let wires = [
        ("route mods", format!("pub mod {m};\n")),
        ("routes", format!("app = app.merge({m}::router());\n")),
        ("paths", format!("pub use super::{m}::paths::*;\n")),
        ("route tables", format!("{m}::ROUTES,\n")),
    ];
    if r.shared {
        plan.shared(&path, file(r));
        for (anchor, text) in wires {
            plan.insert_for(&path, "src/routes.rs", anchor, text);
        }
    } else {
        plan.create(&path, file(r));
        for (anchor, text) in wires {
            plan.insert("src/routes.rs", anchor, text);
        }
    }
}

/// Refuses a feature whose routes module, path helpers or URLs the app already has.
pub fn check_free(
    info: &AppInfo,
    module: Option<&str>,
    paths_fns: &[String],
    url_paths: &[String],
) -> Result<()> {
    if let Some(m) = module
        && info.exists(&path_of(m))
    {
        return Err(Fail::usage(format!(
            "{} already exists: pick another name (or destroy what made it)",
            path_of(m)
        )));
    }
    let sources = routes_md::sources(&info.dir);
    for f in paths_fns {
        if let Some((file, _)) = sources
            .iter()
            .find(|(_, t)| t.contains(&format!("pub fn {f}(")))
        {
            return Err(Fail::usage(format!(
                "{file} already has paths::{f}(): pick another name (or destroy what made it)"
            )));
        }
    }
    let table = routes_md::all(&info.dir);
    for p in url_paths {
        if table.iter().any(|r| r.path == *p) {
            return Err(Fail::usage(format!(
                "ROUTES already has {p}: pick another name"
            )));
        }
    }
    Ok(())
}

/// The `{name}` segments of a path, in order.
pub fn params(path: &str) -> Vec<String> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .map(str::to_owned)
        .collect()
}

/// A path given on the command line: starts with `/`, plain segments or `{snake_case}` params,
/// no repeats.
pub fn check_path(path: &str) -> Result<Vec<String>> {
    let bad = |why: &str| Fail::usage(format!("--path {path}: {why}"));
    if !path.starts_with('/') || path.contains(char::is_whitespace) || path.contains("//") {
        return Err(bad(
            "a path starts with / and has no spaces or empty segments",
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    for seg in path.split('/').skip(1) {
        if seg.contains('{') || seg.contains('}') {
            let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                return Err(bad("a parameter is a whole segment: /blog/{slug}"));
            };
            let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !ok || super::names::RUST_KEYWORDS.contains(&name) {
                return Err(bad(&format!(
                    "{{{name}}}: a parameter is a snake_case name that isn't a Rust keyword"
                )));
            }
            if seen.iter().any(|s| s == name) {
                return Err(bad(&format!("{{{name}}} is given twice")));
            }
            seen.push(name.to_owned());
        } else if !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(bad(&format!("'{seg}': plain segments are a-z, 0-9, - _ .")));
        }
    }
    Ok(seen)
}

/// A path parameter's Rust type: ids are `i64`, everything else a `String`.
pub fn param_type(name: &str) -> &'static str {
    if name == "id" || name.ends_with("_id") {
        "i64"
    } else {
        "String"
    }
}

/// `(slug: &str)` / `(post_id: i64, slug: &str)`: a path helper's arguments.
pub fn helper_args(params: &[String]) -> String {
    params
        .iter()
        .map(|p| {
            if param_type(p) == "i64" {
                format!("{p}: i64")
            } else {
                format!("{p}: &str")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `paths::x("example")`-style arguments a test passes: a value no record has.
pub fn sample_args(params: &[String]) -> String {
    params
        .iter()
        .map(|p| {
            if param_type(p) == "i64" {
                "1".to_owned()
            } else {
                "\"example\"".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A `paths::` helper for `url`: `pub fn name(slug: &str) -> String { format!("/blog/{slug}") }`.
pub fn helper(name: &str, url: &str) -> String {
    let ps = params(url);
    if ps.is_empty() {
        format!("pub fn {name}() -> String {{\n    \"{url}\".into()\n}}\n")
    } else {
        format!(
            "pub fn {name}({}) -> String {{\n    format!(\"{url}\")\n}}\n",
            helper_args(&ps)
        )
    }
}

/// The extractor for a handler: `Path(slug): Path<String>`, `Path((a, b)): Path<(i64, String)>`.
pub fn extractor(params: &[String]) -> Option<String> {
    match params {
        [] => None,
        [one] => Some(format!("Path({one}): Path<{}>", param_type(one))),
        many => Some(format!(
            "Path(({})): Path<({})>",
            many.join(", "),
            many.iter()
                .map(|p| param_type(p))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_params_are_checked_and_typed() {
        assert_eq!(check_path("/blog/{slug}").unwrap(), vec!["slug"]);
        assert_eq!(
            check_path("/u/{user_id}/p/{slug}").unwrap(),
            vec!["user_id", "slug"]
        );
        for bad in [
            "blog",
            "/blog/{Slug}",
            "/blog/x{slug}",
            "/a/{a}/{a}",
            "/a b",
            "/{type}",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        assert_eq!(
            helper("blog_show", "/blog/{slug}"),
            "pub fn blog_show(slug: &str) -> String {\n    format!(\"/blog/{slug}\")\n}\n"
        );
        assert_eq!(
            extractor(&["post_id".into(), "slug".into()]).unwrap(),
            "Path((post_id, slug)): Path<(i64, String)>"
        );
        assert_eq!(sample_args(&["id".into(), "slug".into()]), "1, \"example\"");
    }

    #[test]
    fn a_routes_file_has_its_router_helpers_and_rows() {
        let r = Routes {
            module: "posts".into(),
            what: "Posts".into(),
            mounts: vec![Mount::new(
                "/posts",
                &[
                    ("get", "crate::controllers::posts::index"),
                    ("post", "crate::controllers::posts::create"),
                ],
            )],
            paths: helper("posts", "/posts"),
            rows: vec![(
                "GET".into(),
                "/posts".into(),
                "posts::index".into(),
                "User".into(),
            )],
            shared: false,
        };
        let f = file(&r);
        assert!(f.contains("use axum::{Router, routing::get};"), "{f}");
        assert!(f.contains(".route(\"/posts\", get(crate::controllers::posts::index).post(crate::controllers::posts::create))"));
        assert!(f.contains("    pub fn posts() -> String {\n"));
        let parsed = routes_md::parse(&f);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].access, "user");
    }
}
